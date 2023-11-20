//! Monitoring API for filesystem events.
//!
//! Fanotify is a Linux-only API to monitor filesystems events.
//!
//! Additional capabilities compared to the `inotify` API include the ability to
//! monitor all of the objects in a mounted filesystem, the ability to make
//! access permission decisions, and the possibility to read or modify files
//! before access by other applications.
//!
//! For more documentation, please read
//! [fanotify(7)](https://man7.org/linux/man-pages/man7/fanotify.7.html).

use crate::{NixPath, Result};
use crate::errno::Errno;
use crate::unistd::{read, write};
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::mem::{MaybeUninit, size_of};
use std::ptr;

libc_bitflags! {
    /// Mask for defining which events shall be listened with
    /// [`fanotify_mark`](fn.fanotify_mark.html) and for querying notifications.
    pub struct MaskFlags: u64 {
        /// File was accessed.
        FAN_ACCESS;
        /// File was modified.
        FAN_MODIFY;
        /// Metadata has changed. Since Linux 5.1.
        FAN_ATTRIB;
        /// Writtable file was closed.
        FAN_CLOSE_WRITE;
        /// Unwrittable file was closed.
        FAN_CLOSE_NOWRITE;
        /// File was opened.
        FAN_OPEN;
        /// File was moved from X. Since Linux 5.1.
        FAN_MOVED_FROM;
        /// File was moved to Y. Since Linux 5.1.
        FAN_MOVED_TO;
        /// Subfile was created. Since Linux 5.1.
        FAN_CREATE;
        /// Subfile was deleted. Since Linux 5.1.
        FAN_DELETE;
        /// Self was deleted. Since Linux 5.1.
        FAN_DELETE_SELF;
        /// Self was moved. Since Linux 5.1.
        FAN_MOVE_SELF;
        /// File was opened for execution. Since Linux 5.0.
        FAN_OPEN_EXEC;

        /// Event queue overflowed.
        FAN_Q_OVERFLOW;
        /// Filesystem error. Since Linux 5.16.
        FAN_FS_ERROR;

        /// Permission to open file was requested.
        FAN_OPEN_PERM;
        /// Permission to access file was requested.
        FAN_ACCESS_PERM;
        /// Permission to open file for execution was requested. Since Linux 5.0.
        FAN_OPEN_EXEC_PERM;

        /// Interested in child events.
        FAN_EVENT_ON_CHILD;

        /// File was renamed. Since Linux 5.17.
        FAN_RENAME;

        /// Event occurred against dir.
        FAN_ONDIR;

        /// Combination of `FAN_CLOSE_WRITE` and `FAN_CLOSE_NOWRITE`.
        FAN_CLOSE;
        /// Combination of `FAN_MOVED_FROM` and `FAN_MOVED_TO`.
        FAN_MOVE;
    }
}

libc_bitflags! {
    /// Configuration options for [`fanotify_init`](fn.fanotify_init.html).
    pub struct InitFlags: libc::c_uint {
        /// Close-on-exec flag set on the file descriptor.
        FAN_CLOEXEC;
        /// Nonblocking flag set on the file descriptor.
        FAN_NONBLOCK;

        /// Receipt of events notifications.
        FAN_CLASS_NOTIF;
        /// Receipt of events for permission decisions, after they contain final
        /// data.
        FAN_CLASS_CONTENT;
        /// Receipt of events for permission decisions, before they contain
        /// final data.
        FAN_CLASS_PRE_CONTENT;

        /// Remove the limit of 16384 events for the event queue.
        FAN_UNLIMITED_QUEUE;
        /// Remove the limit of 8192 marks.
        FAN_UNLIMITED_MARKS;
    }
}

libc_bitflags! {
    /// File status flags for fanotify events file descriptors.
    pub struct OFlags: libc::c_uint {
        /// Read only access.
        O_RDONLY as libc::c_uint;
        /// Write only access.
        O_WRONLY as libc::c_uint;
        /// Read and write access.
        O_RDWR as libc::c_uint;
        /// Support for files exceeded 2 GB.
        O_LARGEFILE as libc::c_uint;
        /// Close-on-exec flag for the file descriptor. Since Linux 3.18.
        O_CLOEXEC as libc::c_uint;
        /// Append mode for the file descriptor.
        O_APPEND as libc::c_uint;
        /// Synchronized I/O data integrity completion.
        O_DSYNC as libc::c_uint;
        /// No file last access time update.
        O_NOATIME as libc::c_uint;
        /// Nonblocking mode for the file descriptor.
        O_NONBLOCK as libc::c_uint;
        /// Synchronized I/O file integrity completion.
        O_SYNC as libc::c_uint;
    }
}

libc_bitflags! {
    /// Configuration options for [`fanotify_mark`](fn.fanotify_mark.html).
    pub struct MarkFlags: libc::c_uint {
        /// Add the events to the marks.
        FAN_MARK_ADD;
        /// Remove the events to the marks.
        FAN_MARK_REMOVE;
        /// Don't follow symlinks, mark them.
        FAN_MARK_DONT_FOLLOW;
        /// Raise an error if filesystem to be marked is not a directory.
        FAN_MARK_ONLYDIR;
        /// Events added to or removed from the marks.
        FAN_MARK_IGNORED_MASK;
        /// Ignore mask shall survive modify events.
        FAN_MARK_IGNORED_SURV_MODIFY;
        /// Remove all marks.
        FAN_MARK_FLUSH;
        /// Do not pin inode object in the inode cache. Since Linux 5.19.
        FAN_MARK_EVICTABLE;
        /// Events added to or removed from the marks. Since Linux 6.0.
        FAN_MARK_IGNORE;

        /// Default flag.
        FAN_MARK_INODE;
        /// Mark the mount specified by pathname.
        FAN_MARK_MOUNT;
        /// Mark the filesystem specified by pathname. Since Linux 4.20.
        FAN_MARK_FILESYSTEM;

        /// Combination of `FAN_MARK_IGNORE` and `FAN_MARK_IGNORED_SURV_MODIFY`.
        FAN_MARK_IGNORE_SURV;
    }
}

/// Compile version number of fanotify API.
pub const FANOTIFY_METADATA_VERSION: u8 = libc::FANOTIFY_METADATA_VERSION;

#[derive(Debug)]
/// Abstract over `libc::fanotify_event_metadata`, which represents an event
/// received via `Fanotify::read_events`.
pub struct FanotifyEvent {
    /// Version number for the structure. It must be compared to
    /// `FANOTIFY_METADATA_VERSION` to verify compile version and runtime
    /// version does match. It can be done with the
    /// `FanotifyEvent::check_version` method.
    pub version: u8,
    /// Mask flags of the events.
    pub mask: MaskFlags,
    /// The file descriptor of the event. If the value is `None` when reading
    /// from the fanotify group, this event is to notify that a group queue
    /// overflow occured.
    pub fd: Option<OwnedFd>,
    /// PID of the process that caused the event. TID in case flag
    /// `FAN_REPORT_TID` was set at group initialization.
    pub pid: i32,
}

impl FanotifyEvent {
    /// Checks that compile fanotify API version is equal to the version of the
    /// event.
    pub fn check_version(&self) -> bool {
        self.version == FANOTIFY_METADATA_VERSION
    }
}

#[derive(Debug)]
/// Abstraction over the structure to be sent to allow or deny a given event.
pub struct FanotifyResponse<'e> {
    /// A borrow of the file descriptor from the structure `FanotifyEvent`.
    pub fd: BorrowedFd<'e>,
    /// Indication whether or not the permission is to be granted.
    pub response: Response,
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
/// Response to be wrapped in `FanotifyResponse` and sent to the `Fanotify`
/// group to allow or deny an event.
pub enum Response {
    /// Allow the event.
    Allow,
    /// Deny the event.
    Deny,
}

/// A fanotify group. This is also a file descriptor that can feed to other
/// interfaces consuming file descriptors.
#[derive(Debug)]
pub struct Fanotify {
    fd: OwnedFd,
}

impl Fanotify {
    /// Initialize a new fanotify group.
    ///
    /// Returns a Result containing a Fanotify instance.
    ///
    /// For more information, see [fanotify_init(2)](https://man7.org/linux/man-pages/man7/fanotify_init.2.html).
    pub fn init(flags: InitFlags, event_f_flags: OFlags) -> Result<Fanotify> {
        let res = Errno::result(unsafe {
            libc::fanotify_init(flags.bits(), event_f_flags.bits())
        });
        res.map(|fd| Fanotify { fd: unsafe { OwnedFd::from_raw_fd(fd) }})
    }

    /// Add, remove, or modify an fanotify mark on a filesystem object.
    /// If `dirfd` is `None`, `AT_FDCWD` is used.
    ///
    /// Returns a Result containing either `()` on success or errno otherwise.
    ///
    /// For more information, see [fanotify_mark(2)](https://man7.org/linux/man-pages/man7/fanotify_mark.2.html).
    pub fn mark<P: ?Sized + NixPath>(
        &self,
        flags: MarkFlags,
        mask: MaskFlags,
        dirfd: Option<RawFd>,
        path: Option<&P>,
    ) -> Result<()> {
        fn with_opt_nix_path<P, T, F>(p: Option<&P>, f: F) -> Result<T>
        where
            P: ?Sized + NixPath,
            F: FnOnce(*const libc::c_char) -> T,
        {
            match p {
                Some(path) => path.with_nix_path(|p_str| f(p_str.as_ptr())),
                None => Ok(f(std::ptr::null())),
            }
        }

        let res = with_opt_nix_path(path, |p| unsafe {
            libc::fanotify_mark(
                self.fd.as_raw_fd(),
                flags.bits(),
                mask.bits(),
                dirfd.unwrap_or(libc::AT_FDCWD),
                p,
            )
        })?;

        Errno::result(res).map(|_| ())
    }

    /// Read incoming events from the fanotify group.
    ///
    /// Returns a Result containing either a `Vec` of events on success or errno
    /// otherwise.
    ///
    /// # Errors
    ///
    /// Possible errors can be those that are explicitly listed in
    /// [fanotify(2)](https://man7.org/linux/man-pages/man7/fanotify.2.html) in
    /// addition to the possible errors caused by `read` call.
    /// In particular, `EAGAIN` is returned when no event is available on a
    /// group that has been initialized with the flag `InitFlags::FAN_NONBLOCK`,
    /// thus making this method nonblocking.
    pub fn read_events(&self) -> Result<Vec<FanotifyEvent>> {
        let metadata_size = size_of::<libc::fanotify_event_metadata>();
        const BUFSIZ: usize = 4096;
        let mut buffer = [0u8; BUFSIZ];
        let mut events = Vec::new();
        let mut offset = 0;

        let nread = read(self.fd.as_raw_fd(), &mut buffer)?;

        while (nread - offset) >= metadata_size {
            let metadata = unsafe {
                let mut metadata =
                    MaybeUninit::<libc::fanotify_event_metadata>::uninit();
                ptr::copy_nonoverlapping(
                    buffer.as_ptr().add(offset),
                    metadata.as_mut_ptr() as *mut u8,
                    (BUFSIZ - offset).min(metadata_size),
                );
                metadata.assume_init()
            };

            let fd = (metadata.fd != libc::FAN_NOFD).then(|| unsafe {
                OwnedFd::from_raw_fd(metadata.fd)
            });

            events.push(FanotifyEvent {
                version: metadata.vers,
                mask: MaskFlags::from_bits_truncate(metadata.mask),
                fd,
                pid: metadata.pid,
            });

            offset += metadata.event_len as usize;
        }

        Ok(events)
    }

    /// Write an event response on the fanotify group.
    ///
    /// Returns a Result containing either `()` on success or errno otherwise.
    ///
    /// # Errors
    ///
    /// Possible errors can be those that are explicitly listed in
    /// [fanotify(2)](https://man7.org/linux/man-pages/man7/fanotify.2.html) in
    /// addition to the possible errors caused by `write` call.
    /// In particular, `EAGAIN` or `EWOULDBLOCK` is returned when no event is
    /// available on a group that has been initialized with the flag
    /// `InitFlags::FAN_NONBLOCK`, thus making this method nonblocking.
    pub fn write_response(&self, response: FanotifyResponse) -> Result<()> {
        let response_value = match response.response {
            Response::Allow => libc::FAN_ALLOW,
            Response::Deny => libc::FAN_DENY,
        };
        let resp = libc::fanotify_response {
            fd: response.fd.as_raw_fd(),
            response: response_value,
        };
        write(
            self.fd.as_fd(),
            unsafe {
                std::slice::from_raw_parts(
                    (&resp as *const _) as *const u8,
                    size_of::<libc::fanotify_response>(),
                )
            },
        )?;
        Ok(())
    }
}

impl FromRawFd for Fanotify {
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        Fanotify { fd: OwnedFd::from_raw_fd(fd) }
    }
}

impl AsFd for Fanotify {
    fn as_fd(&'_ self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}
