use super::*;
use crate::syscalls::*;

/// ### `path_link()`
/// Create a hard link
/// Inputs:
/// - `Fd old_fd`
///     The directory relative to which the `old_path` is
/// - `LookupFlags old_flags`
///     Flags to control how `old_path` is understood
/// - `const char *old_path`
///     String containing the old file path
/// - `u32 old_path_len`
///     Length of the `old_path` string
/// - `Fd new_fd`
///     The directory relative to which the `new_path` is
/// - `const char *new_path`
///     String containing the new file path
/// - `u32 old_path_len`
///     Length of the `new_path` string
#[instrument(level = "trace", skip_all, fields(%old_fd, %new_fd, old_path = field::Empty, new_path = field::Empty, follow_symlinks = false), ret)]
pub fn path_link<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    old_fd: WasiFd,
    old_flags: LookupFlags,
    old_path: WasmPtr<u8, M>,
    old_path_len: M::Offset,
    new_fd: WasiFd,
    new_path: WasmPtr<u8, M>,
    new_path_len: M::Offset,
) -> Result<Errno, WasiError> {
    WasiEnv::do_pending_operations(&mut ctx)?;

    if old_flags & __WASI_LOOKUP_SYMLINK_FOLLOW != 0 {
        Span::current().record("follow_symlinks", true);
    }
    let env = ctx.data();
    let (memory, mut state, inodes) = unsafe { env.get_memory_and_wasi_state_and_inodes(&ctx, 0) };
    let mut old_path_str = unsafe { get_input_str_ok!(&memory, old_path, old_path_len) };
    Span::current().record("old_path", old_path_str.as_str());
    let mut new_path_str = unsafe { get_input_str_ok!(&memory, new_path, new_path_len) };
    Span::current().record("new_path", new_path_str.as_str());

    wasi_try_ok!(path_link_internal(
        &mut ctx,
        old_fd,
        old_flags,
        &old_path_str,
        new_fd,
        &new_path_str
    ));
    let env = ctx.data();

    #[cfg(feature = "journal")]
    if env.enable_journal {
        JournalEffector::save_path_link(
            &mut ctx,
            old_fd,
            old_flags,
            old_path_str,
            new_fd,
            new_path_str,
        )
        .map_err(|err| {
            tracing::error!("failed to save path hard link event - {}", err);
            WasiError::Exit(ExitCode::from(Errno::Fault))
        })?;
    }

    Ok(Errno::Success)
}

pub(crate) fn path_link_internal(
    ctx: &mut FunctionEnvMut<'_, WasiEnv>,
    old_fd: WasiFd,
    old_flags: LookupFlags,
    old_path: &str,
    new_fd: WasiFd,
    new_path: &str,
) -> Result<(), Errno> {
    let env = ctx.data();
    let (memory, mut state, inodes) = unsafe { env.get_memory_and_wasi_state_and_inodes(&ctx, 0) };
    let source_fd = state.fs.get_fd(old_fd)?;
    let target_fd = state.fs.get_fd(new_fd)?;

    if !source_fd.inner.rights.contains(Rights::PATH_LINK_SOURCE)
        || !target_fd.inner.rights.contains(Rights::PATH_LINK_TARGET)
    {
        return Err(Errno::Access);
    }

    Span::current().record("old_path", old_path);
    Span::current().record("new_path", new_path);

    let source_inode = state.fs.get_inode_at_path(
        inodes,
        old_fd,
        old_path,
        old_flags & __WASI_LOOKUP_SYMLINK_FOLLOW != 0,
    )?;
    let target_path_arg = std::path::PathBuf::from(new_path);
    let (target_parent_inode, new_entry_name) =
        state
            .fs
            .get_parent_inode_at_path(inodes, new_fd, &target_path_arg, false)?;

    if source_inode.stat.write().unwrap().st_nlink == Linkcount::MAX {
        return Err(Errno::Mlink);
    }
    {
        let mut guard = target_parent_inode.write();
        match guard.deref_mut() {
            Kind::Dir { entries, .. } => {
                if entries.contains_key(&new_entry_name) {
                    return Err(Errno::Exist);
                }
                entries.insert(new_entry_name, source_inode.clone());
            }
            Kind::Root { .. } => return Err(Errno::Inval),
            Kind::File { .. }
            | Kind::Symlink { .. }
            | Kind::Buffer { .. }
            | Kind::Socket { .. }
            | Kind::PipeTx { .. }
            | Kind::PipeRx { .. }
            | Kind::DuplexPipe { .. }
            | Kind::EventNotifications { .. }
            | Kind::Epoll { .. } => return Err(Errno::Notdir),
        }
    }
    source_inode.stat.write().unwrap().st_nlink += 1;

    Ok(())
}
