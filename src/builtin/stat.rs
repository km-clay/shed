#![expect(clippy::unnecessary_cast)]
use std::{fmt, mem, os::unix::fs::MetadataExt};

use nix::{
  libc,
  sys::{stat, statfs, statvfs},
};

use crate::{
  builtin::opt::OptSpec,
  errln,
  expand::escape,
  match_loop, opt, outln, sherr,
  state::vars::VarStr,
  util::{
    self,
    error::{ShErr, ShResult, ShResultExt},
    strops::{ByteCursor, SliceCursor},
  },
};

// File-type bits, normalized to `u32`. The `libc::S_IF*` constants are
// `u16` on some targets (macOS) and `u32` on others (Linux); `st_mode` in
// `FileInfo` is always `u32`, so these give portable match patterns.
const S_IFREG: u32 = libc::S_IFREG as u32;
const S_IFDIR: u32 = libc::S_IFDIR as u32;
const S_IFLNK: u32 = libc::S_IFLNK as u32;
const S_IFCHR: u32 = libc::S_IFCHR as u32;
const S_IFBLK: u32 = libc::S_IFBLK as u32;
const S_IFSOCK: u32 = libc::S_IFSOCK as u32;
const S_IFIFO: u32 = libc::S_IFIFO as u32;

#[derive(Clone, Copy)]
enum Base {
  Decimal,
  Octal,
  Hex,
}

#[derive(Clone, Copy)]
enum TimeDisplay {
  EpochSeconds,
  Readable,
}

#[derive(Clone, Copy)]
enum StatDisplay {
  Machine(Base),
  Human,
}

#[derive(Clone, Copy)]
enum FileTime {
  Birth(TimeDisplay),
  Access(TimeDisplay),
  Modify(TimeDisplay),
  StatChange(TimeDisplay),
}

#[derive(Clone, Copy)]
enum Device {
  MajorNumber,
  MinorNumber,
  MajorDevType(Base),
  MinorDevType(Base),
  DevType(Base),
}

enum FileFmt {
  Literal(VarStr),
  Perms(StatDisplay),
  AllocBlocks,
  BlockSize,
  SecCtx, // SELinux thing
  HexMode,
  FileType,
  Gid,
  GidName,
  HardLinks,
  Inode,
  MountPnt,
  Filename,
  QuotedFilename,
  IoHint,
  FileSize(StatDisplay),
  DevType(Device),
  Uid,
  UidName,
  Time(FileTime),
}

struct FileInfo {
  name: VarStr,
  st_ino: u64,
  st_nlink: u64,
  st_mode: u32,
  st_uid: u32,
  st_gid: u32,
  st_dev: u64,
  st_rdev: u64,
  st_size: i64,
  st_blksize: i64,
  st_blocks: i64,
  st_atime: i64,
  st_mtime: i64,
  st_ctime: i64,
  st_btime: Option<i64>,
  st_btime_nsec: Option<i64>,
}

impl FileInfo {
  fn new(deref: bool, name: VarStr) -> ShResult<Self> {
    let stat = if deref {
      stat::stat(&*name.to_str_lossy())
    } else {
      stat::lstat(&*name.to_str_lossy())
    }
    .map_err(|e| {
      sherr!(
        ExecFail,
        "stat: Failed to stat '{}': {e}",
        name.to_str_lossy()
      )
    })?;

    let mut info = Self {
      name,
      st_ino: stat.st_ino as u64,
      st_nlink: stat.st_nlink as u64,
      st_mode: stat.st_mode as u32,
      st_uid: stat.st_uid,
      st_gid: stat.st_gid,
      st_dev: stat.st_dev as u64,
      st_rdev: stat.st_rdev as u64,
      st_size: stat.st_size as i64,
      st_blksize: stat.st_blksize as i64,
      st_blocks: stat.st_blocks as i64,
      st_atime: stat.st_atime as i64,
      st_mtime: stat.st_mtime as i64,
      st_ctime: stat.st_ctime as i64,
      st_btime: None, // need to get this separately
      st_btime_nsec: None,
    };
    info.set_btime();
    Ok(info)
  }

  fn fmt_mode(&self, f: &mut impl fmt::Write, display: StatDisplay) -> fmt::Result {
    match display {
      StatDisplay::Machine(base) => {
        let mode = self.st_mode & 0o7777;
        match base {
          Base::Decimal => write!(f, "{mode}"),
          Base::Octal => write!(f, "{mode:04o}"),
          Base::Hex => write!(f, "{mode:x}"),
        }
      }
      StatDisplay::Human => {
        let mode = self.st_mode;
        let ty = match mode & 0o170_000 {
          S_IFREG => '-',
          S_IFDIR => 'd',
          S_IFLNK => 'l',
          S_IFCHR => 'c',
          S_IFBLK => 'b',
          S_IFSOCK => 's',
          S_IFIFO => 'p',
          _ => '?',
        };
        write!(f, "{ty}")?;

        let other = mode & 0o007;
        let group = mode & 0o070;
        let owner = mode & 0o700;
        let bit_grps = [other, group, owner];
        for (i, grp) in bit_grps.iter().enumerate().rev() {
          let bits = grp >> (3 * i); // shift down to the leading triple
          let x_mask = match i {
            0 => 0o1000,
            1 => 0o2000,
            2 => 0o4000,
            _ => unreachable!(),
          };

          let exec = bits & 0o1 != 0;
          let special = mode & x_mask != 0;
          let sp = if i == 0 { 't' } else { 's' };
          let x_ch = match (special, exec) {
            (false, false) => '-',
            (false, true) => 'x',
            (true, true) => sp,
            (true, false) => sp.to_ascii_uppercase(),
          };

          if bits & 0o4 != 0 {
            write!(f, "r")?;
          } else {
            write!(f, "-")?;
          }
          if bits & 0o2 != 0 {
            write!(f, "w")?;
          } else {
            write!(f, "-")?;
          }
          write!(f, "{x_ch}")?;
        }
        Ok(())
      }
    }
  }

  fn fmt_filesize(&self, f: &mut impl fmt::Write, display: StatDisplay) -> fmt::Result {
    let size = self.st_size;
    match display {
      StatDisplay::Machine(base) => match base {
        Base::Decimal => write!(f, "{size}"),
        Base::Octal => write!(f, "{size:o}"),
        Base::Hex => write!(f, "{size:x}"),
      },
      StatDisplay::Human => {
        let mut size = size as f64;
        let units = ["B", "K", "M", "G", "T", "P", "E"];
        let mut unit = 0;
        while size >= 1024.0 && unit < units.len() - 1 {
          size /= 1024.0;
          unit += 1;
        }
        if unit == 0 {
          write!(f, "{:.0}{}", size, units[unit])
        } else {
          write!(f, "{:.1}{}", size, units[unit])
        }
      }
    }
  }

  fn fmt_filetype(&self, f: &mut impl fmt::Write) -> fmt::Result {
    let ty = match self.st_mode & 0o170_000 {
      S_IFREG => {
        if self.st_size == 0 {
          "regular empty file"
        } else {
          "regular file"
        }
      }
      S_IFDIR => "directory",
      S_IFLNK => "symbolic link",
      S_IFCHR => "character special file",
      S_IFBLK => "block special file",
      S_IFSOCK => "socket",
      S_IFIFO => "fifo",
      _ => "weird file",
    };
    write!(f, "{ty}")
  }

  #[cfg(not(linux_like))]
  fn fmt_sec_ctx(&self, f: &mut impl fmt::Write) -> fmt::Result {
    write!(f, "?")
  }

  #[cfg(linux_like)]
  fn fmt_sec_ctx(&self, f: &mut impl fmt::Write) -> fmt::Result {
    let Ok(path) = std::ffi::CString::new(self.name.as_bytes()) else {
      return write!(f, "?");
    };
    let attr = c"security.selinux";
    let len = unsafe { libc::lgetxattr(path.as_ptr(), attr.as_ptr(), std::ptr::null_mut(), 0) };
    if len <= 0 {
      return write!(f, "?");
    }

    let mut buf = vec![0u8; len as usize];
    let n = unsafe {
      libc::lgetxattr(
        path.as_ptr(),
        attr.as_ptr(),
        buf.as_mut_ptr().cast(),
        buf.len(),
      )
    };
    if n <= 0 {
      return write!(f, "?");
    }

    buf.truncate(n as usize);
    if buf.last() == Some(&0) {
      buf.pop();
    }

    write!(f, "{}", String::from_utf8_lossy(&buf))
  }

  fn fmt_gid_name(&self, f: &mut impl fmt::Write) -> fmt::Result {
    match nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(self.st_gid)) {
      Ok(Some(group)) => write!(f, "{}", group.name),
      _ => write!(f, "{}", self.st_gid),
    }
  }

  fn fmt_uid_name(&self, f: &mut impl fmt::Write) -> fmt::Result {
    match nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(self.st_uid)) {
      Ok(Some(user)) => write!(f, "{}", user.name),
      _ => write!(f, "{}", self.st_uid),
    }
  }

  fn fmt_mount_pnt(&self, f: &mut impl fmt::Write) -> fmt::Result {
    let Ok(canon) = std::fs::canonicalize(&self.name) else {
      return write!(f, "?");
    };
    let Ok(meta) = std::fs::metadata(&canon) else {
      return write!(f, "?");
    };
    let dev = meta.dev();

    let mut mp = canon.as_path();
    while let Some(parent) = mp.parent() {
      match std::fs::metadata(parent) {
        Ok(p) if p.dev() == dev => mp = parent, // climb
        _ => break,
      }
    }

    write!(f, "{}", mp.display())
  }

  fn fmt_quoted_name(&self, f: &mut impl fmt::Write) -> fmt::Result {
    let quoted = escape::shell_quote(&self.name.to_str_lossy());

    if self.st_mode & 0o170_000 == S_IFLNK
      && let Ok(tgt) = std::fs::read_link(&self.name)
    {
      write!(f, "{quoted} -> {}", tgt.display())
    } else {
      write!(f, "{quoted}")
    }
  }

  #[expect(clippy::similar_names)]
  fn fmt_dev_type(&self, f: &mut impl fmt::Write, device: Device) -> fmt::Result {
    let major_dev = libc::major(self.st_dev as libc::dev_t);
    let major_rdev = libc::major(self.st_rdev as libc::dev_t);
    let minor_dev = libc::minor(self.st_dev as libc::dev_t);
    let minor_rdev = libc::minor(self.st_rdev as libc::dev_t);

    match device {
      Device::MajorNumber => write!(f, "{major_dev}"),
      Device::MinorNumber => write!(f, "{minor_dev}"),
      Device::MajorDevType(base) => match base {
        Base::Decimal => write!(f, "{major_rdev}"),
        Base::Octal => write!(f, "{major_rdev:o}"),
        Base::Hex => write!(f, "{major_rdev:x}"),
      },
      Device::MinorDevType(base) => match base {
        Base::Decimal => write!(f, "{minor_rdev}"),
        Base::Octal => write!(f, "{minor_rdev:o}"),
        Base::Hex => write!(f, "{minor_rdev:x}"),
      },
      Device::DevType(base) => match base {
        Base::Decimal => write!(f, "{major_dev}:{minor_dev}"),
        Base::Octal => write!(f, "{major_dev:o}:{minor_dev:o}"),
        Base::Hex => write!(f, "{major_dev:x}:{minor_dev:x}"),
      },
    }
  }

  fn fmt_time(&self, f: &mut impl fmt::Write, time: FileTime) -> fmt::Result {
    let (time, display) = match time {
      FileTime::Access(time_display) => (self.st_atime, time_display),
      FileTime::Modify(time_display) => (self.st_mtime, time_display),
      FileTime::StatChange(time_display) => (self.st_ctime, time_display),
      FileTime::Birth(time_display) => match self.st_btime {
        None => return write!(f, "-"),
        Some(btime) => (btime, time_display),
      },
    };

    match display {
      TimeDisplay::EpochSeconds => write!(f, "{time}"),
      TimeDisplay::Readable => {
        let time = chrono::DateTime::from_timestamp(time, 0).unwrap_or_default();
        write!(f, "{}", time.format("%Y-%m-%d %H:%M:%S"))
      }
    }
  }

  fn set_btime(&mut self) {
    let Ok(meta) = std::fs::symlink_metadata(&self.name) else {
      return;
    };
    let Ok(created) = meta.created() else { return };
    let btime_dur = created
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap_or_default();
    self.st_btime = Some(btime_dur.as_secs() as i64);
    self.st_btime_nsec = Some(i64::from(btime_dur.subsec_nanos()));
  }
}

impl FileFmt {
  fn format(&self, f: &mut impl fmt::Write, stat: &FileInfo) -> fmt::Result {
    match self {
      FileFmt::Literal(var_str)/**/=> write!(f, "{var_str}"),
      FileFmt::AllocBlocks /*---*/ => write!(f, "{}", stat.st_blocks),
      FileFmt::HexMode /*-------*/ => write!(f, "{:x}", stat.st_mode),
      FileFmt::Gid /*===========*/ => write!(f, "{}", stat.st_gid),
      FileFmt::HardLinks /*-----*/ => write!(f, "{}", stat.st_nlink),
      FileFmt::Inode /*=========*/ => write!(f, "{}", stat.st_ino),
      FileFmt::Filename /*------*/ => write!(f, "{}", stat.name),
      FileFmt::BlockSize /*=====*/ |
      FileFmt::IoHint /*========*/ => write!(f, "{}", stat.st_blksize),
      FileFmt::Uid /*===========*/ => write!(f, "{}", stat.st_uid),
      FileFmt::FileSize(disp) /**/ => stat.fmt_filesize(f, *disp),
      FileFmt::FileType /*------*/ => stat.fmt_filetype(f),
      FileFmt::Perms(stat_display) => stat.fmt_mode(f, *stat_display),
      FileFmt::SecCtx /*--------*/ => stat.fmt_sec_ctx(f),
      FileFmt::GidName /*=======*/ => stat.fmt_gid_name(f),
      FileFmt::MountPnt /*------*/ => stat.fmt_mount_pnt(f),
      FileFmt::QuotedFilename /**/ => stat.fmt_quoted_name(f),
      FileFmt::UidName /*-------*/ => stat.fmt_uid_name(f),
      FileFmt::DevType(device)/*=*/=> stat.fmt_dev_type(f, *device),
      FileFmt::Time(file_time)/*-*/=> stat.fmt_time(f, *file_time),
    }
  }
}

struct FileFmtArgs(Vec<FileFmt>);

impl FileFmtArgs {
  fn parse(bytes: &[u8]) -> Result<Self, ShErr> {
    let mut cur = SliceCursor::new(bytes);
    let mut literal = util::scratch_buf();
    let mut args = vec![];

    match_loop!(cur.next_byte() => b, {
      b'%' => {
        let Some(b) = cur.next_byte() else {
          return Err(sherr!(ExecFail, "stat: Incomplete format specifier"));
        };
        if b == b'%' {
          literal.push(b'%');
          continue
        }
        args.push(FileFmt::Literal(mem::take(&mut literal).as_slice().into()));

        match b {
          b'a' => args.push(FileFmt::Perms(StatDisplay::Machine(Base::Octal))),
          b'A' => args.push(FileFmt::Perms(StatDisplay::Human)),
          b'b' => args.push(FileFmt::AllocBlocks),
          b'B' => args.push(FileFmt::BlockSize),
          b'C' => args.push(FileFmt::SecCtx),
          b'd' |
          b'D' |
          b'R' => args.push(FileFmt::DevType(Device::DevType(Base::Hex))),
          first @ (b'H' | b'L') => {
            let Some(next) = cur.next_byte() else {
              return Err(sherr!(ExecFail, "stat: Incomplete format specifier"));
            };

            match (first, next) {
              (b'H', b'd') => args.push(FileFmt::DevType(Device::MajorNumber)),
              (b'H', b'r') => args.push(FileFmt::DevType(Device::MajorDevType(Base::Decimal))),
              (b'L', b'd') => args.push(FileFmt::DevType(Device::MinorNumber)),
              (b'L', b'r') => args.push(FileFmt::DevType(Device::MinorDevType(Base::Decimal))),
              _ => return Err(sherr!(ExecFail, "stat: Unsupported format specifier '{}' for '%{}'", next as char, first as char)),
            }
          }
          b'f' => args.push(FileFmt::HexMode),
          b'F' => args.push(FileFmt::FileType),
          b'g' => args.push(FileFmt::Gid),
          b'G' => args.push(FileFmt::GidName),
          b'h' => args.push(FileFmt::HardLinks),
          b'i' => args.push(FileFmt::Inode),
          b'm' => args.push(FileFmt::MountPnt),
          b'n' => args.push(FileFmt::Filename),
          b'N' => args.push(FileFmt::QuotedFilename),
          b'o' => args.push(FileFmt::IoHint),
          b's' => args.push(FileFmt::FileSize(StatDisplay::Machine(Base::Decimal))),
          b'S' => args.push(FileFmt::FileSize(StatDisplay::Human)),
          b'r' => args.push(FileFmt::DevType(Device::DevType(Base::Decimal))),
          b't' => args.push(FileFmt::DevType(Device::MajorDevType(Base::Hex))),
          b'T' => args.push(FileFmt::DevType(Device::MinorDevType(Base::Hex))),
          b'u' => args.push(FileFmt::Uid),
          b'U' => args.push(FileFmt::UidName),
          b'w' => args.push(FileFmt::Time(FileTime::Birth(TimeDisplay::Readable))),
          b'W' => args.push(FileFmt::Time(FileTime::Birth(TimeDisplay::EpochSeconds))),
          b'x' => args.push(FileFmt::Time(FileTime::Access(TimeDisplay::Readable))),
          b'X' => args.push(FileFmt::Time(FileTime::Access(TimeDisplay::EpochSeconds))),
          b'y' => args.push(FileFmt::Time(FileTime::Modify(TimeDisplay::Readable))),
          b'Y' => args.push(FileFmt::Time(FileTime::Modify(TimeDisplay::EpochSeconds))),
          b'z' => args.push(FileFmt::Time(FileTime::StatChange(TimeDisplay::Readable))),
          b'Z' => args.push(FileFmt::Time(FileTime::StatChange(TimeDisplay::EpochSeconds))),
          _ => {
            return Err(sherr!(ExecFail, "stat: Unsupported format specifier '%{}'", b as char));
          }
        }
      }
      _ => literal.push(b),
    });

    if !literal.is_empty() {
      args.push(FileFmt::Literal(mem::take(&mut literal).as_slice().into()));
    }

    Ok(Self(args))
  }
}

enum FsFmt {
  Literal(VarStr),
  FreeBlocksForNonRoot,
  FreeBlocks,
  TotalBlocks,
  TotalNodes,
  FreeNodes,
  FsId,
  MaxNameLen,
  FileName,
  BlockSize,
  FundamentalBs,
  FsType(StatDisplay),
}

struct FsInfo {
  block_size: u64,
  fundamental_bs: u64,
  total_blks: u64,
  free_blks: u64,
  avail_blks: u64,
  total_nodes: u64,
  free_nodes: u64,
  fs_id: u64,
  name_max: u64,
  fs_type_id: Option<u64>,      // numeric magic; linux only
  fs_type_name: Option<String>, // human-readable type, if resolvable
}

impl FsInfo {
  /// Gather filesystem info for `path`. Numeric fields come from the
  /// portable `statvfs`; the filesystem type is resolved separately since
  /// that part is platform-specific.
  fn for_path(path: &str) -> nix::Result<Self> {
    let v = statvfs::statvfs(path)?;
    let (fs_type_id, fs_type_name) = fs_type_of(path);
    Ok(Self {
      block_size: v.block_size() as u64,
      fundamental_bs: v.fragment_size() as u64,
      total_blks: v.blocks() as u64,
      free_blks: v.blocks_free() as u64,
      avail_blks: v.blocks_available() as u64,
      total_nodes: v.files() as u64,
      free_nodes: v.files_free() as u64,
      fs_id: v.filesystem_id() as u64,
      name_max: v.name_max() as u64,
      fs_type_id,
      fs_type_name,
    })
  }

  fn fmt_fs_type(&self, f: &mut impl fmt::Write, display: StatDisplay) -> fmt::Result {
    match display {
      StatDisplay::Machine(base) => {
        let id = self.fs_type_id.unwrap_or(0);
        match base {
          Base::Decimal => write!(f, "{id}"),
          Base::Octal => write!(f, "{id:o}"),
          Base::Hex => write!(f, "{id:x}"),
        }
      }
      StatDisplay::Human => match &self.fs_type_name {
        Some(name) => write!(f, "{name}"),
        None => write!(f, "UNKNOWN"),
      },
    }
  }
}

/// Resolve the filesystem type at `path` into `(numeric magic, human name)`.
#[cfg(linux_like)]
fn fs_type_of(path: &str) -> (Option<u64>, Option<String>) {
  match statfs::statfs(path) {
    Ok(s) => {
      let ty = s.filesystem_type();
      (Some(ty.0 as u64), Some(fs_type_readable(ty).to_string()))
    }
    Err(_) => (None, None),
  }
}

#[cfg(not(linux_like))]
fn fs_type_of(path: &str) -> (Option<u64>, Option<String>) {
  match statfs::statfs(path) {
    Ok(s) => (None, Some(s.filesystem_type_name().to_string())),
    Err(_) => (None, None),
  }
}

impl FsFmt {
  fn format(&self, f: &mut impl fmt::Write, name: &str, stat: &FsInfo) -> fmt::Result {
    match self {
      FsFmt::Literal(var_str )/**/=> write!(f, "{var_str}"),
      FsFmt::FileName /*=======*/ => write!(f, "{name}"),
      FsFmt::FreeBlocksForNonRoot => write!(f, "{}", stat.avail_blks),
      FsFmt::FreeBlocks /*=====*/ => write!(f, "{}", stat.free_blks),
      FsFmt::TotalBlocks /*----*/ => write!(f, "{}", stat.total_blks),
      FsFmt::TotalNodes /*=====*/ => write!(f, "{}", stat.total_nodes),
      FsFmt::FreeNodes /*------*/ => write!(f, "{}", stat.free_nodes),
      FsFmt::FsId /*===========*/ => write!(f, "{}", stat.fs_id),
      FsFmt::MaxNameLen /*-----*/ => write!(f, "{}", stat.name_max),
      FsFmt::BlockSize /*------*/ => write!(f, "{}", stat.block_size),
      FsFmt::FundamentalBs /*==*/ => write!(f, "{}", stat.fundamental_bs),
      FsFmt::FsType(stat_display) => stat.fmt_fs_type(f, *stat_display)
    }
  }
}

#[cfg(linux_like)]
fn fs_type_readable(id: statfs::FsType) -> &'static str {
  match id {
    statfs::ADFS_SUPER_MAGIC => "adfs",
    statfs::AFFS_SUPER_MAGIC => "affs",
    statfs::AFS_SUPER_MAGIC => "afs",
    statfs::AUTOFS_SUPER_MAGIC => "autofs",
    statfs::BPF_FS_MAGIC => "bpf",
    statfs::BTRFS_SUPER_MAGIC => "btrfs",
    statfs::CGROUP2_SUPER_MAGIC => "cgroup2",
    statfs::CGROUP_SUPER_MAGIC => "cgroup",
    statfs::CODA_SUPER_MAGIC => "coda",
    statfs::CRAMFS_MAGIC => "cramfs",
    statfs::DEBUGFS_MAGIC => "debugfs",
    statfs::DEVPTS_SUPER_MAGIC => "devpts",
    statfs::ECRYPTFS_SUPER_MAGIC => "ecryptfs",
    statfs::EFS_SUPER_MAGIC => "efs",
    statfs::EXT2_SUPER_MAGIC => "ext2/ext3/ext4", // all ext filesystems use the same number for some reason
    statfs::F2FS_SUPER_MAGIC => "f2fs",
    statfs::FUSE_SUPER_MAGIC => "fuse",
    statfs::FUTEXFS_SUPER_MAGIC => "futexfs",
    statfs::HOSTFS_SUPER_MAGIC => "hostfs",
    statfs::HPFS_SUPER_MAGIC => "hpfs",
    statfs::HUGETLBFS_MAGIC => "hugetlbfs",
    statfs::ISOFS_SUPER_MAGIC => "isofs",
    statfs::JFFS2_SUPER_MAGIC => "jffs2",
    statfs::MINIX2_SUPER_MAGIC => "minix2",
    statfs::MINIX2_SUPER_MAGIC2 => "minix2",
    statfs::MINIX3_SUPER_MAGIC => "minix3",
    statfs::MINIX_SUPER_MAGIC => "minix",
    statfs::MINIX_SUPER_MAGIC2 => "minix",
    statfs::MSDOS_SUPER_MAGIC => "msdos",
    statfs::NCP_SUPER_MAGIC => "ncp",
    statfs::NFS_SUPER_MAGIC => "nfs",
    statfs::NILFS_SUPER_MAGIC => "nilfs",
    statfs::NSFS_MAGIC => "nsfs",
    statfs::OCFS2_SUPER_MAGIC => "ocfs2",
    statfs::OPENPROM_SUPER_MAGIC => "openprom",
    statfs::OVERLAYFS_SUPER_MAGIC => "overlayfs",
    statfs::PROC_SUPER_MAGIC => "proc",
    statfs::QNX4_SUPER_MAGIC => "qnx4",
    statfs::QNX6_SUPER_MAGIC => "qnx6",
    statfs::RDTGROUP_SUPER_MAGIC => "rdtgroup",
    statfs::REISERFS_SUPER_MAGIC => "reiserfs",
    statfs::SECURITYFS_MAGIC => "securityfs",
    statfs::SELINUX_MAGIC => "selinux",
    statfs::SMACK_MAGIC => "smack",
    statfs::SMB_SUPER_MAGIC => "smb",
    statfs::SYSFS_MAGIC => "sysfs",
    statfs::TMPFS_MAGIC => "tmpfs",
    statfs::TRACEFS_MAGIC => "tracefs",
    statfs::UDF_SUPER_MAGIC => "udf",
    statfs::USBDEVICE_SUPER_MAGIC => "usbdevice",
    statfs::XENFS_SUPER_MAGIC => "xenfs",
    // nix excludes this magic on musl and ohos
    #[cfg(all(not(target_env = "musl"), not(target_env = "ohos")))]
    statfs::XFS_SUPER_MAGIC => "xfs",
    _ => "UNKNOWN",
  }
}

struct FsFmtArgs(Vec<FsFmt>);

impl FsFmtArgs {
  fn parse(bytes: &[u8]) -> Result<Self, ShErr> {
    let mut cur = SliceCursor::new(bytes);
    let mut literal = util::scratch_buf();
    let mut args = vec![];

    match_loop!(cur.next_byte() => b, {
      b'%' => {
        let Some(b) = cur.next_byte() else {
          return Err(sherr!(ExecFail, "stat: Incomplete format specifier"));
        };
        if b == b'%' {
          literal.push(b'%');
          continue
        }
        args.push(FsFmt::Literal(mem::take(&mut literal).as_slice().into()));

        match b {
          b'a' => args.push(FsFmt::FreeBlocksForNonRoot),
          b'b' => args.push(FsFmt::TotalBlocks),
          b'c' => args.push(FsFmt::TotalNodes),
          b'd' => args.push(FsFmt::FreeNodes),
          b'f' => args.push(FsFmt::FreeBlocks),
          b'i' => args.push(FsFmt::FsId),
          b'l' => args.push(FsFmt::MaxNameLen),
          b'n' => args.push(FsFmt::FileName),
          b's' => args.push(FsFmt::BlockSize),
          b'S' => args.push(FsFmt::FundamentalBs),
          b't' => args.push(FsFmt::FsType(StatDisplay::Machine(Base::Hex))),
          b'T' => args.push(FsFmt::FsType(StatDisplay::Human)),
          _ => {
            return Err(sherr!(ExecFail, "stat: Unsupported format specifier '%{}'", b as char));
          }
        }
      }
      _ => literal.push(b),
    });

    if !literal.is_empty() {
      args.push(FsFmt::Literal(mem::take(&mut literal).as_slice().into()));
    }

    Ok(Self(args))
  }
}

pub(super) struct Stat;
impl super::Builtin for Stat {
  fn strict_opts(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("dereference" | b'L'),
      opt!("file-system" | b'f'),
      opt!("terse" | b't'),
      opt!("format" | b'c', 1),
    ]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let mut deref = false;
    let mut fs_stat = false;
    let mut terse = false;
    let mut format: Option<VarStr> = None;

    let (arg_vec, opts) = args.take_argv();

    if arg_vec.is_empty() {
      return Err(sherr!(ExecFail @ args.cmd_span(), "stat: Missing file operand"));
    }

    for opt in opts {
      match opt.key() {
        "format" => {
          format = Some(opt.value()?.into());
        }
        "dereference" => deref = true,
        "file-system" => fs_stat = true,
        "terse" => terse = true,

        _ => return Err(sherr!(ExecFail, "stat: Unsupported option '{opt}'")),
      }
    }

    if terse && format.is_none() {
      if fs_stat {
        format = Some(Self::TERSE_FS_FMT.into());
      } else {
        format = Some(Self::TERSE_FILE_FMT.into());
      }
    }

    let format = if fs_stat {
      format.unwrap_or_else(|| Self::DEFAULT_FS_FMT.into())
    } else {
      format.unwrap_or_else(|| Self::DEFAULT_FILE_FMT.into())
    };

    let mut buf = String::new();
    let mut status = 0;

    if fs_stat {
      let fmt_args = FsFmtArgs::parse(format.as_bytes())?;
      for (arg, _) in arg_vec {
        let stat = match FsInfo::for_path(&arg.to_str_lossy()) {
          Ok(stat) => stat,
          Err(e) => {
            errln!("stat: Failed to statfs '{}': {e}", arg.to_str_lossy());
            status = 1;
            continue;
          }
        };
        for fmt in &fmt_args.0 {
          fmt.format(&mut buf, &arg.to_str_lossy(), &stat)?;
        }

        outln!("{}", mem::take(&mut buf));
      }
    } else {
      let fmt_args = FileFmtArgs::parse(format.as_bytes())?;
      for (arg, span) in arg_vec {
        let Ok(stat) = FileInfo::new(deref, arg.to_str_lossy().into()).promote_err(span) else {
          errln!("stat: Failed to stat '{}'", arg.to_str_lossy());
          status = 1;
          continue;
        };
        for fmt in &fmt_args.0 {
          fmt.format(&mut buf, &stat)?;
        }

        outln!("{}", mem::take(&mut buf));
      }
    }

    util::with_status(status)
  }
}

impl Stat {
  const DEFAULT_FILE_FMT: &str = "  File: %N\n  Size: %S\t\tBlocks: %b\tIO Block: %o\t%F\nDevice: %Hd,%Ld\tInode: %i\t\tLinks: %h\nAccess: (%a/%A)  Uid: (%u/%U)  Gid: (%g/%G)\nAccess: %x\nModify: %y\nChange: %z\n Birth: %w";
  const DEFAULT_FS_FMT: &str = "  File: %N\n    ID: %i\tNamelen: %l\t Type: %t\nBlock size: %s\tFundamental block size: %S\nBlocks: Total: %b\tFree: %f\tAvailable: %a\nInodes: Total: %c\tFree: %d";
  const TERSE_FILE_FMT: &str = "%n %s %b %f %u %g %D %i %h %t %T %X %Y %Z %W %o";
  const TERSE_FS_FMT: &str = "%n %i %l %t %s %S %b %f %a %c %d";
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::tests::testutil::TestGuard;
  use std::io::Write;
  use std::os::unix::fs::PermissionsExt;
  use std::path::Path;
  use tempfile::{NamedTempFile, TempDir};

  /// Build a `FileInfo` for `path` and render `fmt` against it.
  fn render(deref: bool, path: &str, fmt: &str) -> String {
    let info = FileInfo::new(deref, path.into()).unwrap();
    let args = FileFmtArgs::parse(fmt.as_bytes()).unwrap();
    let mut out = String::new();
    for f in &args.0 {
      f.format(&mut out, &info).unwrap();
    }
    out
  }

  fn chmod(path: &Path, mode: u32) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms).unwrap();
  }

  #[test]
  fn perms_symbolic_and_octal() {
    let _g = TestGuard::new();
    let f = NamedTempFile::new().unwrap();
    chmod(f.path(), 0o644);
    let p = f.path().to_str().unwrap();
    assert_eq!(render(false, p, "%A"), "-rw-r--r--");
    assert_eq!(render(false, p, "%a"), "0644");
  }

  #[test]
  fn perms_executable() {
    let _g = TestGuard::new();
    let f = NamedTempFile::new().unwrap();
    chmod(f.path(), 0o755);
    assert_eq!(
      render(false, f.path().to_str().unwrap(), "%A"),
      "-rwxr-xr-x"
    );
  }

  #[test]
  fn sticky_directory() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    chmod(dir.path(), 0o1777);
    assert_eq!(
      render(false, dir.path().to_str().unwrap(), "%A"),
      "drwxrwxrwt"
    );
  }

  #[test]
  fn file_type_words() {
    let _g = TestGuard::new();
    let mut f = NamedTempFile::new().unwrap();
    // A zero-byte regular file is GNU's "regular empty file".
    assert_eq!(
      render(false, f.path().to_str().unwrap(), "%F"),
      "regular empty file"
    );
    f.write_all(b"data").unwrap();
    f.flush().unwrap();
    assert_eq!(
      render(false, f.path().to_str().unwrap(), "%F"),
      "regular file"
    );
    let dir = TempDir::new().unwrap();
    assert_eq!(
      render(false, dir.path().to_str().unwrap(), "%F"),
      "directory"
    );
  }

  #[test]
  fn symlink_lstat_vs_dereference() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("target");
    std::fs::write(&target, "hi").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let lp = link.to_str().unwrap();
    // default (lstat) sees the link itself
    assert_eq!(render(false, lp, "%F"), "symbolic link");
    assert!(render(false, lp, "%A").starts_with('l'));
    // -L follows to the regular file
    assert_eq!(render(true, lp, "%F"), "regular file");
  }

  #[test]
  fn size_and_links() {
    let _g = TestGuard::new();
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"hello").unwrap();
    f.flush().unwrap();
    let p = f.path().to_str().unwrap();
    assert_eq!(render(false, p, "%s"), "5");
    assert_eq!(render(false, p, "%h"), "1");
  }

  #[test]
  fn literal_and_percent_escape() {
    let _g = TestGuard::new();
    let f = NamedTempFile::new().unwrap();
    let p = f.path().to_str().unwrap();
    assert_eq!(render(false, p, "x%%y"), "x%y");
    assert_eq!(render(false, p, "name=%n"), format!("name={p}"));
  }

  #[test]
  fn uid_matches_current_user() {
    let _g = TestGuard::new();
    let f = NamedTempFile::new().unwrap();
    let uid = nix::unistd::Uid::current().to_string();
    assert_eq!(render(false, f.path().to_str().unwrap(), "%u"), uid);
  }

  #[test]
  fn unknown_and_incomplete_specifiers_error() {
    let _g = TestGuard::new();
    assert!(FileFmtArgs::parse(b"%").is_err());
    assert!(FileFmtArgs::parse(b"%q").is_err());
  }

  #[test]
  #[cfg(linux_like)]
  fn fs_type_readable_names() {
    // ext2/ext3/ext4 all share magic 0xEF53, so the magic can't distinguish them.
    assert_eq!(fs_type_readable(statfs::EXT4_SUPER_MAGIC), "ext2/ext3/ext4");
    assert_eq!(fs_type_readable(statfs::TMPFS_MAGIC), "tmpfs");
    assert_eq!(fs_type_readable(statfs::FsType(0)), "UNKNOWN");
  }
}
