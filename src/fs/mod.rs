use std::fmt;
use std::sync::{Arc, Mutex};
use std::ffi;
use std::collections::HashMap;
use std::u64;
use time;
use fuse;
use libc;

use cache;
use chunk;

mod utils;

const TTL: time::Timespec = time::Timespec { sec: 1, nsec: 0 };

#[derive(Debug)]
pub enum Error {
}
type FilesystemResult<T> = Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug)]
pub struct Filesystem<C, M> {
    cache: Arc<Mutex<C>>,
    chunk_manager: M,
    uid: u32,
    gid: u32,
    handles: HashMap<u64, cache::File>,
    handle_id: u64
}

impl<C, M> Filesystem<C, M>
    where C: cache::MetadataCache + Send + 'static,
          M: chunk::Manager
{
    pub fn new(cache: Arc<Mutex<C>>, chunk_manger: M, uid: u32, gid: u32) -> FilesystemResult<Filesystem<C, M>> {
        Ok(Filesystem {
               cache: cache,
               chunk_manager: chunk_manger,
               uid: uid,
               gid: gid,
               handles: HashMap::new(),
               handle_id: 0,
           })
    }
}

impl<C, M> fuse::Filesystem for Filesystem<C, M>
    where C: cache::MetadataCache + Send + 'static,
          M: chunk::Manager
{
    fn getattr(&mut self, _req: &fuse::Request, inode: u64, reply: fuse::ReplyAttr) {
        let file: cache::File = match self.cache
                  .lock()
                  .unwrap()
                  .get_file(inode) {
            Ok(file) => file,
            Err(cause) => {
                warn!("{}", cause);

                return reply.error(libc::ENOENT);
            }
        };

        reply.attr(&TTL,
                   &utils::create_attrs_for_file(&file, self.uid, self.gid))
    }

    fn readdir(&mut self,
               _req: &fuse::Request,
               inode: u64,
               _fh: u64,
               offset: u64,
               mut reply: fuse::ReplyDirectory) {
        if offset != 0 {
            return reply.error(libc::ENOENT);
        }

        let files: Vec<cache::File> = match self.cache
                  .lock()
                  .unwrap()
                  .get_child_files_by_inode(inode) {
            Ok(files) => files,
            Err(cause) => {
                warn!("{}", cause);

                return reply.error(libc::ENOENT);
            }
        };

        reply.add(1, 0, fuse::FileType::Directory, ".");
        reply.add(1, 1, fuse::FileType::Directory, "..");

        let mut i = 2;
        for file in files {
            reply.add(file.inode.unwrap(),
                      i,
                      utils::get_filetype_for_file(&file),
                      &file.name);
            i += 1;
        }

        reply.ok()
    }

    fn lookup(&mut self,
              _req: &fuse::Request,
              parent_inode: u64,
              name: &ffi::OsStr,
              reply: fuse::ReplyEntry) {
        let file: cache::File =
            match self.cache
                      .lock()
                      .unwrap()
                      .get_child_file_by_inode_and_name(parent_inode,
                                                        name.to_str().unwrap().to_owned()) {
                Ok(file) => file,
                Err(cause) => {
                    trace!("{:?}", cause);

                    return reply.error(libc::ENOENT);
                }
            };

        reply.entry(&TTL,
                    &utils::create_attrs_for_file(&file, self.uid, self.gid),
                    0)
    }

    fn open(&mut self, _req: &fuse::Request, inode: u64, _flags: u32, reply: fuse::ReplyOpen) {
        let file: cache::File = match self.cache
                  .lock()
                  .unwrap()
                  .get_file(inode) {
            Ok(file) => file,
            Err(cause) => {
                warn!("{:?}", cause);

                return reply.error(libc::EIO);
            }
        };

        let fh = self.handle_id.clone();

        if self.handle_id + 1 > u64::MAX {
            self.handle_id = 0
        } else {
            self.handle_id += 1;
        }

        self.handles.insert(fh, file);
        reply.opened(fh, 0)
    }

    fn release(&mut self, _req: &fuse::Request, _ino: u64, fh: u64, _flags: u32, _lock_owner: u64, _flush: bool, reply: fuse::ReplyEmpty) {
        self.handles.remove(&fh);
        reply.ok()
    }

    fn read(&mut self,
            _req: &fuse::Request,
            inode: u64,
            fh: u64,
            offset: u64,
            size: u32,
            reply: fuse::ReplyData) {

        let file = match self.handles.get(&fh) {
            Some(file) => file,
            None => {
                warn!("Could not open file handle with inode {} / fh {}", inode, fh);

                return reply.error(libc::EIO);
            },
        };

        self.chunk_manager.get_chunk(&file.download_url, offset, size as u64, |result| {
            match result {
                Ok(chunk) => reply.data(&chunk),
                Err(cause) => {
                    warn!("{:?}", cause);

                    reply.error(libc::EIO);
                }
            }
        });    
    }
}