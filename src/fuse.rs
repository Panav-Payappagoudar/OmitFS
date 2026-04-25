use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, Request,
};
use libc::{EEXIST, EIO, ENOENT};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};
use tracing::{error, info};

use crate::db::OmitDb;
use crate::embedding::EmbeddingEngine;

const TTL: Duration = Duration::from_secs(1);
const ROOT_INO: u64 = 1;

// ─── Internal inode representation ───────────────────────────────────────────

enum Node {
    Root,
    VirtualDir {
        name:     String,
        children: Vec<(u64, String)>, // (child_ino, filename)
    },
    VirtualFile {
        name:          String,
        physical_path: PathBuf,
        parent_ino:    u64,
    },
}

// ─── Filesystem struct ────────────────────────────────────────────────────────

pub struct OmitFs {
    rt:         tokio::runtime::Handle,
    db:         Arc<OmitDb>,
    engine:     Arc<Mutex<EmbeddingEngine>>,
    raw_dir:    PathBuf,
    nodes:      HashMap<u64, Node>,
    next_inode: u64,
}

impl OmitFs {
    pub fn new(
        db:      Arc<OmitDb>,
        engine:  Arc<Mutex<EmbeddingEngine>>,
        raw_dir: PathBuf,
    ) -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(ROOT_INO, Node::Root);
        Self {
            rt: tokio::runtime::Handle::current(),
            db,
            engine,
            raw_dir,
            nodes,
            next_inode: 2,
        }
    }

    fn alloc_ino(&mut self) -> u64 {
        let ino = self.next_inode;
        self.next_inode += 1;
        ino
    }

    fn build_attr(&self, ino: u64) -> Option<FileAttr> {
        let node = self.nodes.get(&ino)?;

        let mut attr = FileAttr {
            ino,
            size:    0,
            blocks:  0,
            atime:   UNIX_EPOCH,
            mtime:   UNIX_EPOCH,
            ctime:   UNIX_EPOCH,
            crtime:  UNIX_EPOCH,
            kind:    FileType::Directory,
            perm:    0o755,
            nlink:   2,
            uid:     unsafe { libc::getuid() },
            gid:     unsafe { libc::getgid() },
            rdev:    0,
            flags:   0,
            blksize: 512,
        };

        match node {
            Node::Root | Node::VirtualDir { .. } => {
                attr.kind  = FileType::Directory;
                attr.perm  = 0o755;
                attr.size  = 4096;
                attr.nlink = 2;
            }
            Node::VirtualFile { physical_path, .. } => {
                attr.kind  = FileType::RegularFile;
                attr.perm  = 0o644;
                attr.nlink = 1;
                if let Ok(meta) = std::fs::metadata(physical_path) {
                    attr.size = meta.len();
                    if let Ok(mtime) = meta.modified() {
                        let dur = mtime
                            .duration_since(std::time::SystemTime::UNIX_EPOCH)
                            .unwrap_or_default();
                        let ts = UNIX_EPOCH + dur;
                        attr.mtime = ts;
                        attr.atime = ts;
                        attr.ctime = ts;
                    }
                }
            }
        }
        Some(attr)
    }

    /// Returns `(child_ino, index_in_children_vec)` if `name` exists inside `dir_ino`.
    fn find_child(&self, dir_ino: u64, name: &str) -> Option<(u64, usize)> {
        if let Some(Node::VirtualDir { children, .. }) = self.nodes.get(&dir_ino) {
            children
                .iter()
                .enumerate()
                .find(|(_, (_, n))| n == name)
                .map(|(idx, (ino, _))| (*ino, idx))
        } else {
            None
        }
    }
}

// ─── FUSE trait implementation ────────────────────────────────────────────────

impl Filesystem for OmitFs {
    // ── lookup ──────────────────────────────────────────────────────────────
    // Invoked whenever the kernel resolves a path component.
    // At root level it triggers a semantic vector search.
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name_str = name.to_string_lossy().to_string();

        // Case 1: lookup inside an already-materialised virtual dir → find child
        if let Some(Node::VirtualDir { children, .. }) = self.nodes.get(&parent) {
            for &(child_ino, ref child_name) in children.iter() {
                if child_name == &name_str {
                    if let Some(attr) = self.build_attr(child_ino) {
                        reply.entry(&TTL, &attr, 0);
                        return;
                    }
                }
            }
            reply.error(ENOENT);
            return;
        }

        // Case 2: lookup at root → semantic query
        if parent != ROOT_INO {
            reply.error(ENOENT);
            return;
        }

        info!("Semantic query: «{}»", name_str);

        // Return cached dir if query already materialised
        let existing: Option<u64> = self.nodes.iter().find_map(|(&ino, node)| {
            if let Node::VirtualDir { name: dir_name, .. } = node {
                if dir_name == &name_str { Some(ino) } else { None }
            } else {
                None
            }
        });
        if let Some(ino) = existing {
            if let Some(attr) = self.build_attr(ino) {
                reply.entry(&TTL, &attr, 0);
                return;
            }
        }

        // Embed the query locally
        let vector = {
            let mut eng = self.engine.lock().unwrap();
            match eng.embed(&name_str) {
                Ok(v) => v,
                Err(e) => {
                    error!("Embedding failed for «{}»: {}", name_str, e);
                    reply.error(EIO);
                    return;
                }
            }
        };

        // Vector search in LanceDB
        let db = self.db.clone();
        let search_result = self.rt.block_on(async { db.search(vector, 10).await });

        match search_result {
            Ok(files) => {
                let dir_ino = self.alloc_ino();
                let mut children: Vec<(u64, String)> = Vec::new();

                for (filename, physical_path) in files {
                    let file_ino = self.alloc_ino();
                    children.push((file_ino, filename.clone()));
                    self.nodes.insert(
                        file_ino,
                        Node::VirtualFile {
                            name:          filename,
                            physical_path: PathBuf::from(physical_path),
                            parent_ino:    dir_ino,
                        },
                    );
                }

                self.nodes.insert(dir_ino, Node::VirtualDir { name: name_str, children });

                match self.build_attr(dir_ino) {
                    Some(attr) => reply.entry(&TTL, &attr, 0),
                    None       => reply.error(EIO),
                }
            }
            Err(e) => {
                error!("LanceDB search error: {}", e);
                reply.error(EIO);
            }
        }
    }

    // ── getattr ──────────────────────────────────────────────────────────────
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        match self.build_attr(ino) {
            Some(attr) => reply.attr(&TTL, &attr),
            None       => reply.error(ENOENT),
        }
    }

    // ── readdir ──────────────────────────────────────────────────────────────
    fn readdir(
        &mut self,
        _req:    &Request,
        ino:     u64,
        _fh:     u64,
        offset:  i64,
        mut reply: ReplyDirectory,
    ) {
        let mut entries: Vec<(u64, FileType, String)> =
            vec![(ino, FileType::Directory, ".".into())];

        if ino == ROOT_INO {
            entries.push((ROOT_INO, FileType::Directory, "..".into()));
            // Root void is intentionally empty — cd into a concept to populate it
        } else if let Some(node) = self.nodes.get(&ino) {
            match node {
                Node::VirtualDir { children, .. } => {
                    entries.push((ROOT_INO, FileType::Directory, "..".into()));
                    for (child_ino, child_name) in children {
                        entries.push((*child_ino, FileType::RegularFile, child_name.clone()));
                    }
                }
                _ => {
                    reply.error(ENOENT);
                    return;
                }
            }
        } else {
            reply.error(ENOENT);
            return;
        }

        for (i, (e_ino, e_kind, e_name)) in
            entries.into_iter().enumerate().skip(offset as usize)
        {
            if reply.add(e_ino, (i + 1) as i64, e_kind, e_name) {
                break; // buffer full
            }
        }
        reply.ok();
    }

    // ── open ─────────────────────────────────────────────────────────────────
    fn open(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        match self.nodes.get(&ino) {
            Some(Node::VirtualFile { .. }) => reply.opened(0, 0),
            _                             => reply.error(ENOENT),
        }
    }

    // ── read ─────────────────────────────────────────────────────────────────
    // Byte passthrough: reads the physical file and returns the requested slice.
    fn read(
        &mut self,
        _req:        &Request,
        ino:         u64,
        _fh:         u64,
        offset:      i64,
        size:        u32,
        _flags:      i32,
        _lock_owner: Option<u64>,
        reply:       ReplyData,
    ) {
        match self.nodes.get(&ino) {
            Some(Node::VirtualFile { physical_path, .. }) => {
                match std::fs::read(physical_path) {
                    Ok(data) => {
                        let start = offset as usize;
                        if start >= data.len() {
                            reply.data(&[]);
                        } else {
                            let end = (start + size as usize).min(data.len());
                            reply.data(&data[start..end]);
                        }
                    }
                    Err(e) => {
                        error!("read passthrough error (ino {}): {}", ino, e);
                        reply.error(EIO);
                    }
                }
            }
            _ => reply.error(ENOENT),
        }
    }

    // ── unlink (delete) ──────────────────────────────────────────────────────
    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name_str = name.to_string_lossy().to_string();

        let Some((child_ino, idx)) = self.find_child(parent, &name_str) else {
            reply.error(ENOENT);
            return;
        };

        // Delete physical file
        if let Some(Node::VirtualFile { physical_path, .. }) = self.nodes.get(&child_ino) {
            if let Err(e) = std::fs::remove_file(physical_path) {
                error!("unlink: failed to delete {:?}: {}", physical_path, e);
                reply.error(EIO);
                return;
            }
            info!("Deleted physical file: {:?}", physical_path);
        }

        // Remove from parent's children list
        if let Some(Node::VirtualDir { children, .. }) = self.nodes.get_mut(&parent) {
            children.remove(idx);
        }
        self.nodes.remove(&child_ino);
        reply.ok();
    }

    // ── rename (move / relocate) ─────────────────────────────────────────────
    // Handles `mv src dst` inside the virtual mount.
    // Physically moves the file and updates the internal inode map.
    fn rename(
        &mut self,
        _req:      &Request,
        parent:    u64,
        name:      &OsStr,
        newparent: u64,
        newname:   &OsStr,
        _flags:    u32,
        reply:     ReplyEmpty,
    ) {
        let name_str    = name.to_string_lossy().to_string();
        let newname_str = newname.to_string_lossy().to_string();

        // Resolve source
        let Some((src_ino, src_idx)) = self.find_child(parent, &name_str) else {
            reply.error(ENOENT);
            return;
        };

        // Compute destination on disk (always lands in raw_dir)
        let dest_path = self.raw_dir.join(&newname_str);

        if dest_path.exists() {
            reply.error(EEXIST);
            return;
        }

        // Get source physical path
        let src_path = match self.nodes.get(&src_ino) {
            Some(Node::VirtualFile { physical_path, .. }) => physical_path.clone(),
            _ => { reply.error(ENOENT); return; }
        };

        // OS-level rename
        if let Err(e) = std::fs::rename(&src_path, &dest_path) {
            error!("rename {:?} → {:?}: {}", src_path, dest_path, e);
            reply.error(EIO);
            return;
        }
        info!("Renamed {:?} → {:?}", src_path, dest_path);

        // Update node
        if let Some(Node::VirtualFile { name, physical_path, .. }) = self.nodes.get_mut(&src_ino) {
            *name          = newname_str.clone();
            *physical_path = dest_path;
        }

        // Remove from old parent
        if let Some(Node::VirtualDir { children, .. }) = self.nodes.get_mut(&parent) {
            children.remove(src_idx);
        }

        // Insert into new parent (or re-insert at root level — inode map only)
        if let Some(Node::VirtualDir { children, .. }) = self.nodes.get_mut(&newparent) {
            children.push((src_ino, newname_str));
        }

        reply.ok();
    }
}
