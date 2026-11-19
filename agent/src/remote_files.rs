//! Files copied on another device, as this desktop sees them before they
//! are pasted: a read-only FUSE file system in the runtime directory whose
//! files have the right names and sizes and no contents here. What a file
//! manager reads, when the user pastes, is fetched from the other device a
//! piece at a time (`Core::open_remote_files`); nothing is downloaded for a
//! copy that is never pasted, and nothing is kept on this device but what
//! the file manager writes where the user pasted.
//!
//! This is what Windows does with virtual files on its clipboard
//! (`rinflow-windows/agent/docs/clipboard.md`); a Wayland clipboard carries
//! file paths, so here the files need a place in the file system.
//!
//! Each offer (one copy on a peer) is a numbered folder under the mount.
//! The first read opens a lease on the peer, which pins what was copied
//! there for this paste even if either clipboard changes meanwhile; the
//! lease is given back a few seconds after the last file is closed. A read
//! that fails (the peer went away, the user cancelled) fails the rest of
//! that paste rather than starting over in the middle of it.

use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner, MountOption, OpenFlags, ReplyAttr,
    ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, Request,
};
use rinflow_core::{Core, FileManifest, MAX_READ_SIZE, RemoteFileLease};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

/// How long the kernel may remember names and attributes: they never
/// change for an offer, but a withdrawn offer should stop being found.
const TTL: Duration = Duration::from_secs(1);
/// A lease is given back this long after the last file of it was closed
/// (a file manager closes one file before it opens the next).
const LEASE_IDLE: Duration = Duration::from_secs(5);
/// Threads answering the kernel: a read waits for the peer, and file
/// managers read ahead.
const THREADS: usize = 8;
const ROOT: u64 = 1;

enum Kind {
    Folder { children: Vec<u64> },
    File { entry: u32, size: u64 },
}

struct Node {
    offer: u64,
    parent: u64,
    name: String,
    kind: Kind,
}

enum Lease {
    /// Not reading: the next read opens one.
    None,
    /// A read is asking the peer; the others wait for it.
    Opening,
    Open(Arc<RemoteFileLease>),
    /// A read failed: so do the rest, until every file is closed, with
    /// this error.
    Broken(Errno),
}

struct Offer {
    peer: String,
    transfer: u32,
    /// Its folder under the mount, and every node in it.
    folder: u64,
    nodes: Vec<u64>,
    since: SystemTime,
    /// The peer no longer offers it: found only by a paste under way.
    withdrawn: bool,
    /// How far each entry has been read in this paste, for the progress.
    reached: Vec<u64>,
    /// Open files.
    handles: usize,
    idle_since: Instant,
    lease: Lease,
}

impl Offer {
    fn in_use(&self) -> bool {
        self.handles > 0 || matches!(self.lease, Lease::Opening | Lease::Open(_))
    }
}

#[derive(Default)]
struct Tree {
    nodes: HashMap<u64, Node>,
    offers: HashMap<u64, Offer>,
    last_node: u64,
    last_offer: u64,
    last_handle: u64,
}

struct Shared {
    tree: Mutex<Tree>,
    /// Signalled when a lease stops being `Opening`.
    opened: Condvar,
    core: Weak<Core>,
    owner: (u32, u32),
    /// Told how much of a peer's files has been read: its ID, the bytes.
    progress: Progress,
}

/// See [`RemoteFiles::new`].
pub type Progress = Box<dyn Fn(&str, u64) + Send + Sync>;

impl Shared {
    fn attr(&self, tree: &Tree, ino: u64) -> Option<FileAttr> {
        let (kind, size, since) = if ino == ROOT {
            (FileType::Directory, 0, SystemTime::UNIX_EPOCH)
        } else {
            let node = tree.nodes.get(&ino)?;
            let since = tree.offers.get(&node.offer)?.since;
            match node.kind {
                Kind::Folder { .. } => (FileType::Directory, 0, since),
                Kind::File { size, .. } => (FileType::RegularFile, size, since),
            }
        };
        let folder = kind == FileType::Directory;
        Some(FileAttr {
            ino: INodeNo(ino),
            size,
            blocks: size.div_ceil(512),
            atime: since,
            mtime: since,
            ctime: since,
            crtime: since,
            kind,
            // What a file made here would have. File managers copy the
            // mode with the contents, so this is what a pasted file gets:
            // read-only would leave the user unable to save it. The mount
            // itself is read-only, and only this user can reach it.
            perm: if folder { 0o755 } else { 0o644 },
            nlink: if folder { 2 } else { 1 },
            uid: self.owner.0,
            gid: self.owner.1,
            rdev: 0,
            flags: 0,
            blksize: 128 * 1024,
        })
    }

    /// The node `name` in folder `parent`. Offers that were withdrawn and
    /// are not being pasted are not found at the top.
    fn child(&self, tree: &Tree, parent: u64, name: &OsStr) -> Option<u64> {
        let name = name.to_str()?;
        if parent == ROOT {
            return tree
                .offers
                .iter()
                .find(|(id, offer)| id.to_string() == name && (!offer.withdrawn || offer.in_use()))
                .map(|(_, offer)| offer.folder);
        }
        match &tree.nodes.get(&parent)?.kind {
            Kind::Folder { children } => children.iter().copied().find(|child| tree.nodes.get(child).is_some_and(|node| node.name == name)),
            Kind::File { .. } => None,
        }
    }

    /// The lease to read offer `id` with, opened on the peer if this is the
    /// first read of a paste. Blocks while the peer answers.
    fn lease(&self, id: u64) -> Result<Arc<RemoteFileLease>, Errno> {
        let mut tree = self.tree.lock().unwrap();
        loop {
            let offer = tree.offers.get_mut(&id).ok_or(Errno::ESTALE)?;
            match &offer.lease {
                Lease::Open(lease) => return Ok(lease.clone()),
                Lease::Broken(error) => return Err(*error),
                Lease::Opening => tree = self.opened.wait(tree).unwrap(),
                Lease::None => {
                    offer.lease = Lease::Opening;
                    let (peer, transfer) = (offer.peer.clone(), offer.transfer);
                    drop(tree);
                    let opened = match self.core.upgrade() {
                        Some(core) => core.open_remote_files(&peer, transfer),
                        None => Err("the core stopped".to_owned()),
                    };
                    tree = self.tree.lock().unwrap();
                    let result = match (opened, tree.offers.get_mut(&id)) {
                        (Ok(lease), Some(offer)) => {
                            let lease = Arc::new(lease);
                            offer.lease = Lease::Open(lease.clone());
                            log::info!("files from {peer}: reading");
                            Ok(lease)
                        }
                        (Err(error), Some(offer)) => {
                            log::warn!("files from {peer} cannot be read: {error}");
                            offer.lease = Lease::Broken(Errno::EIO);
                            Err(Errno::EIO)
                        }
                        (_, None) => Err(Errno::ESTALE),
                    };
                    self.opened.notify_all();
                    return result;
                }
            }
        }
    }

    /// A read failed: the paste it belongs to is over.
    /// A read of offer `id` failed: the error the file manager is to get
    /// for it and for the rest of this paste.
    fn broke(&self, id: u64, lease: &Arc<RemoteFileLease>, error: &str) -> Errno {
        let errno = errno_for(error);
        let mut tree = self.tree.lock().unwrap();
        if let Some(offer) = tree.offers.get_mut(&id) {
            match &offer.lease {
                Lease::Open(open) if Arc::ptr_eq(open, lease) => {
                    log::warn!("files from {}: {error}", offer.peer);
                    offer.lease = Lease::Broken(errno);
                    lease.finish(error);
                }
                // Another read got there first: its error stands.
                Lease::Broken(first) => return *first,
                _ => {}
            }
        }
        errno
    }

    /// Gives back leases nobody reads with any more, and forgets offers
    /// that were withdrawn once nothing of them is open.
    fn tidy(&self) {
        let mut tree = self.tree.lock().unwrap();
        let mut gone = Vec::new();
        for (&id, offer) in &mut tree.offers {
            if offer.handles > 0 {
                continue;
            }
            match &offer.lease {
                Lease::Open(lease) if offer.idle_since.elapsed() >= LEASE_IDLE => {
                    lease.finish("delivered");
                    offer.lease = Lease::None;
                    offer.reached.fill(0);
                }
                Lease::Broken(_) => {
                    offer.lease = Lease::None;
                    offer.reached.fill(0);
                }
                _ => {}
            }
            if offer.withdrawn && !offer.in_use() {
                gone.push(id);
            }
        }
        for id in gone {
            if let Some(offer) = tree.offers.remove(&id) {
                for node in offer.nodes {
                    tree.nodes.remove(&node);
                }
            }
        }
    }
}

struct Files(Arc<Shared>);

impl Filesystem for Files {
    fn lookup(&self, _request: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let tree = self.0.tree.lock().unwrap();
        match self.0.child(&tree, parent.0, name).and_then(|ino| self.0.attr(&tree, ino)) {
            Some(attr) => reply.entry(&TTL, &attr, Generation(0)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn getattr(&self, _request: &Request, ino: INodeNo, _handle: Option<FileHandle>, reply: ReplyAttr) {
        let tree = self.0.tree.lock().unwrap();
        match self.0.attr(&tree, ino.0) {
            Some(attr) => reply.attr(&TTL, &attr),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn readdir(&self, _request: &Request, ino: INodeNo, _handle: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let tree = self.0.tree.lock().unwrap();
        let mut entries: Vec<(u64, FileType, String)> = Vec::new();
        if ino.0 == ROOT {
            entries.push((ROOT, FileType::Directory, ".".to_owned()));
            entries.push((ROOT, FileType::Directory, "..".to_owned()));
            for (id, offer) in &tree.offers {
                if !offer.withdrawn || offer.in_use() {
                    entries.push((offer.folder, FileType::Directory, id.to_string()));
                }
            }
        } else {
            let Some(Node { parent, kind: Kind::Folder { children }, .. }) = tree.nodes.get(&ino.0) else {
                return reply.error(Errno::ENOTDIR);
            };
            entries.push((ino.0, FileType::Directory, ".".to_owned()));
            entries.push((*parent, FileType::Directory, "..".to_owned()));
            for child in children {
                if let Some(node) = tree.nodes.get(child) {
                    let kind = if matches!(node.kind, Kind::Folder { .. }) { FileType::Directory } else { FileType::RegularFile };
                    entries.push((*child, kind, node.name.clone()));
                }
            }
        }
        for (index, (ino, kind, name)) in entries.into_iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(ino), index as u64 + 1, kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn open(&self, _request: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let mut tree = self.0.tree.lock().unwrap();
        let Some(Node { offer, kind: Kind::File { .. }, .. }) = tree.nodes.get(&ino.0) else {
            return reply.error(Errno::EISDIR);
        };
        let id = *offer;
        let Some(offer) = tree.offers.get_mut(&id).filter(|offer| !offer.withdrawn || offer.in_use()) else {
            return reply.error(Errno::ESTALE);
        };
        offer.handles += 1;
        tree.last_handle += 1;
        reply.opened(FileHandle(tree.last_handle), FopenFlags::empty());
    }

    fn release(
        &self,
        _request: &Request,
        ino: INodeNo,
        _handle: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let mut tree = self.0.tree.lock().unwrap();
        let id = tree.nodes.get(&ino.0).map(|node| node.offer);
        if let Some(offer) = id.and_then(|id| tree.offers.get_mut(&id)) {
            offer.handles = offer.handles.saturating_sub(1);
            offer.idle_since = Instant::now();
        }
        reply.ok();
    }

    fn read(
        &self,
        _request: &Request,
        ino: INodeNo,
        _handle: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let (id, entry, length) = {
            let tree = self.0.tree.lock().unwrap();
            match tree.nodes.get(&ino.0) {
                Some(Node { offer, kind: Kind::File { entry, size: length }, .. }) => (*offer, *entry, *length),
                Some(_) => return reply.error(Errno::EISDIR),
                None => return reply.error(Errno::ENOENT),
            }
        };
        let end = length.min(offset.saturating_add(u64::from(size)));
        if offset >= end {
            return reply.data(&[]);
        }
        let lease = match self.0.lease(id) {
            Ok(lease) => lease,
            Err(error) => return reply.error(error),
        };
        let mut data = Vec::with_capacity((end - offset) as usize);
        while offset + (data.len() as u64) < end {
            let at = offset + data.len() as u64;
            match lease.read(entry, at, ((end - at) as usize).min(MAX_READ_SIZE)) {
                Ok(piece) if !piece.is_empty() => data.extend_from_slice(&piece),
                Ok(_) => break,
                Err(error) => return reply.error(self.0.broke(id, &lease, &error)),
            }
        }
        // The core's own count stalls when reads arrive out of order, as
        // a file manager's read-ahead does: counted here by how far each
        // file has been read.
        let read = {
            let mut tree = self.0.tree.lock().unwrap();
            tree.offers.get_mut(&id).map(|offer| {
                if let Some(reached) = offer.reached.get_mut(entry as usize) {
                    *reached = (*reached).max(offset + data.len() as u64);
                }
                (offer.peer.clone(), offer.reached.iter().sum::<u64>())
            })
        };
        if let Some((peer, received)) = read {
            (self.0.progress)(&peer, received);
        }
        reply.data(&data);
    }
}

/// The files peers offer, mounted when the first offer comes.
pub struct RemoteFiles {
    directory: PathBuf,
    shared: Arc<Shared>,
    session: Option<fuser::BackgroundSession>,
    /// Mounting failed (no FUSE here, as in a Flatpak): not tried again.
    unavailable: bool,
}

impl RemoteFiles {
    /// `directory` is where the files appear; nothing is mounted yet.
    /// `progress` hears how much of a peer's files a paste has read so far.
    pub fn new(directory: PathBuf, core: Weak<Core>, progress: Progress) -> Self {
        // SAFETY: getuid and getgid have no preconditions.
        let owner = unsafe { (libc::getuid(), libc::getgid()) };
        let shared = Arc::new(Shared { tree: Mutex::new(Tree::default()), opened: Condvar::new(), core, owner, progress });
        let tidying = Arc::downgrade(&shared);
        let _ = std::thread::Builder::new().name("rinflow-files".to_owned()).spawn(move || {
            while let Some(shared) = tidying.upgrade() {
                shared.tidy();
                drop(shared);
                std::thread::sleep(Duration::from_secs(1));
            }
        });
        Self { directory, shared, session: None, unavailable: false }
    }

    fn mount(&mut self) -> bool {
        if self.session.is_some() {
            return true;
        }
        if self.unavailable {
            return false;
        }
        // A mount left by an agent that was killed answers nothing.
        if std::fs::metadata(&self.directory).is_err_and(|error| error.raw_os_error() == Some(libc::ENOTCONN)) {
            let _ = std::process::Command::new("fusermount3").arg("-u").arg("-z").arg(&self.directory).status();
        }
        let mut config = fuser::Config::default();
        config.mount_options = vec![
            MountOption::RO,
            MountOption::NoSuid,
            MountOption::NoDev,
            MountOption::NoExec,
            MountOption::NoAtime,
            MountOption::FSName("rinflow".to_owned()),
            MountOption::Subtype("rinflow".to_owned()),
        ];
        config.n_threads = Some(THREADS);
        let mounted = std::fs::create_dir_all(&self.directory).and_then(|()| fuser::spawn_mount(Files(self.shared.clone()), &self.directory, &config));
        match mounted {
            Ok(session) => {
                log::info!("files from other devices appear in {}", self.directory.display());
                self.session = Some(session);
                true
            }
            Err(error) => {
                log::warn!("files from other devices cannot be offered here (FUSE: {error})");
                self.unavailable = true;
                false
            }
        }
    }

    /// Makes the files `peer` copied appear here, in place of what it
    /// offered before. Returns the paths of the top-level files and
    /// folders, for the clipboard; `None` when FUSE is not available or
    /// the manifest names nothing usable.
    pub fn publish(&mut self, peer: &str, manifest: &FileManifest) -> Option<Vec<PathBuf>> {
        if !self.mount() {
            return None;
        }
        let tops = self.publish_tree(peer, manifest);
        (!tops.is_empty()).then_some(tops)
    }

    fn publish_tree(&mut self, peer: &str, manifest: &FileManifest) -> Vec<PathBuf> {
        self.withdraw(peer);
        let mut guard = self.shared.tree.lock().unwrap();
        let tree = &mut *guard;
        tree.last_offer += 1;
        let id = tree.last_offer;
        tree.last_node = tree.last_node.max(ROOT) + 1;
        let folder = tree.last_node;
        tree.nodes.insert(folder, Node { offer: id, parent: ROOT, name: id.to_string(), kind: Kind::Folder { children: Vec::new() } });
        let mut nodes = vec![folder];
        let mut folders: HashMap<String, u64> = HashMap::new();
        let mut tops = Vec::new();
        for (index, entry) in manifest.entries.iter().enumerate() {
            let parts: Vec<&str> = entry.path.split('/').collect();
            if parts.iter().any(|part| part.is_empty() || *part == "." || *part == ".." || part.contains('\0')) {
                log::warn!("files from {peer}: an entry with an unusable name is left out");
                continue;
            }
            // Its folders, made on the way when the manifest did not list them.
            let mut parent = folder;
            let mut path = String::new();
            for (depth, part) in parts.iter().enumerate() {
                if depth > 0 {
                    path.push('/');
                }
                path.push_str(part);
                let last = depth + 1 == parts.len();
                if let Some(&existing) = folders.get(&path) {
                    parent = existing;
                    continue;
                }
                let kind = if last && !entry.dir { Kind::File { entry: index as u32, size: entry.size } } else { Kind::Folder { children: Vec::new() } };
                let is_folder = matches!(kind, Kind::Folder { .. });
                tree.last_node += 1;
                let ino = tree.last_node;
                tree.nodes.insert(ino, Node { offer: id, parent, name: (*part).to_owned(), kind });
                nodes.push(ino);
                if let Some(Node { kind: Kind::Folder { children }, .. }) = tree.nodes.get_mut(&parent) {
                    children.push(ino);
                }
                if depth == 0 {
                    tops.push(self.directory.join(id.to_string()).join(part));
                }
                if is_folder {
                    folders.insert(path.clone(), ino);
                    parent = ino;
                }
            }
        }
        tree.offers.insert(
            id,
            Offer {
                peer: peer.to_owned(),
                transfer: manifest.transfer,
                folder,
                nodes,
                since: SystemTime::now(),
                withdrawn: false,
                reached: vec![0; manifest.entries.len()],
                handles: 0,
                idle_since: Instant::now(),
                lease: Lease::None,
            },
        );
        tops
    }

    /// `peer` no longer offers its files (it copied something else, or is
    /// gone). A paste under way goes on; a new one finds nothing. Returns
    /// whether it had offered any.
    pub fn withdraw(&mut self, peer: &str) -> bool {
        let mut tree = self.shared.tree.lock().unwrap();
        let mut any = false;
        for offer in tree.offers.values_mut().filter(|offer| offer.peer == peer && !offer.withdrawn) {
            offer.withdrawn = true;
            any = true;
        }
        any
    }

    /// Sharing was turned off: nothing is offered, and what is being read
    /// stops (the core has revoked the leases).
    pub fn withdraw_all(&mut self) {
        let mut tree = self.shared.tree.lock().unwrap();
        for offer in tree.offers.values_mut() {
            offer.withdrawn = true;
            if let Lease::Open(lease) = std::mem::replace(&mut offer.lease, Lease::Broken(Errno::ECANCELED)) {
                lease.finish("cancelled");
            }
        }
    }

}

/// What a file manager is told when a read fails. The user stopping the
/// paste (Cancel in the window, or sharing turned off) is `ECANCELED`,
/// which GIO takes for a cancelled operation: GTK file managers stop
/// without an error dialog. Anything else (the peer gone, a timeout, a file
/// changed there) is an I/O error, and the file manager says the copy
/// failed.
fn errno_for(error: &str) -> Errno {
    if error == "cancelled" { Errno::ECANCELED } else { Errno::EIO }
}

/// The paths as a `text/uri-list`: `file://` URLs, one per line.
pub fn uri_list(paths: &[PathBuf]) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut list = String::new();
    for path in paths {
        list.push_str("file://");
        for &byte in path.as_os_str().as_bytes() {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
                list.push(byte as char);
            } else {
                list.push_str(&format!("%{byte:02X}"));
            }
        }
        list.push_str("\r\n");
    }
    list
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_paste_the_user_stopped_is_cancelled_and_any_other_failure_an_error() {
        assert_eq!(super::errno_for("cancelled"), fuser::Errno::ECANCELED);
        assert_eq!(super::errno_for("remote file read timed out"), fuser::Errno::EIO);
        assert_eq!(super::errno_for("heartbeat timeout"), fuser::Errno::EIO);
        assert_eq!(super::errno_for("No such file or directory (os error 2)"), fuser::Errno::EIO);
    }

    use super::*;
    use rinflow_core::FileEntry;

    fn manifest(entries: &[(&str, u64, bool)]) -> FileManifest {
        FileManifest {
            transfer: 7,
            total_size: entries.iter().map(|entry| entry.1).sum(),
            entries: entries.iter().map(|(path, size, dir)| FileEntry { path: (*path).to_owned(), size: *size, dir: *dir }).collect(),
        }
    }

    /// The tree a manifest makes, without mounting it.
    fn published(entries: &[(&str, u64, bool)]) -> (RemoteFiles, Vec<PathBuf>) {
        let mut files = RemoteFiles::new(PathBuf::from("/run/files"), Weak::new(), Box::new(|_, _| {}));
        let tops = files.publish_tree("mac", &manifest(entries));
        (files, tops)
    }

    #[test]
    fn a_manifest_becomes_folders_and_files_with_their_sizes() {
        let (files, tops) = published(&[("报告", 0, true), ("报告/a b.txt", 5, false), ("报告/图/c.png", 9, false), ("note.txt", 3, false)]);
        assert_eq!(tops, [PathBuf::from("/run/files/1/报告"), PathBuf::from("/run/files/1/note.txt")]);
        let tree = files.shared.tree.lock().unwrap();
        let shared = &files.shared;
        let offer = shared.child(&tree, ROOT, OsStr::new("1")).unwrap();
        let folder = shared.child(&tree, offer, OsStr::new("报告")).unwrap();
        assert_eq!(shared.attr(&tree, folder).unwrap().kind, FileType::Directory);
        let file = shared.child(&tree, folder, OsStr::new("a b.txt")).unwrap();
        assert_eq!(shared.attr(&tree, file).unwrap().size, 5);
        // A folder the manifest did not list by itself.
        let inner = shared.child(&tree, folder, OsStr::new("图")).unwrap();
        let picture = shared.child(&tree, inner, OsStr::new("c.png")).unwrap();
        assert!(matches!(tree.nodes[&picture].kind, Kind::File { entry: 2, size: 9 }));
        assert!(shared.child(&tree, offer, OsStr::new("missing")).is_none());
    }

    #[test]
    fn names_that_would_leave_the_folder_are_left_out() {
        let (_, tops) = published(&[("../up", 1, false), ("a//b", 1, false), ("ok", 1, false)]);
        assert_eq!(tops, [PathBuf::from("/run/files/1/ok")]);
    }

    #[test]
    fn a_withdrawn_offer_is_not_found_and_is_forgotten() {
        let (mut files, _) = published(&[("a", 1, false)]);
        assert!(files.withdraw("mac"));
        assert!(!files.withdraw("mac"));
        {
            let tree = files.shared.tree.lock().unwrap();
            assert!(files.shared.child(&tree, ROOT, OsStr::new("1")).is_none());
        }
        files.shared.tidy();
        let tree = files.shared.tree.lock().unwrap();
        assert!(tree.offers.is_empty() && tree.nodes.is_empty());
    }

    #[test]
    fn paths_become_file_urls() {
        assert_eq!(uri_list(&[PathBuf::from("/run/user/1000/rinflow/files/1/a b 报.txt")]), "file:///run/user/1000/rinflow/files/1/a%20b%20%E6%8A%A5.txt\r\n");
    }
}
