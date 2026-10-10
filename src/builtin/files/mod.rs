use crate::sub_command;

use super::SubCommand;

mod chmod;
mod chown;
mod link;
mod mkdir;
mod readlink;
mod realpath;
mod rename;
mod rmdir;
mod symlink;
mod touch;
mod truncate;
mod unlink;

pub(super) struct Fs;
impl super::BuiltinRouter for Fs {
  fn name(&self) -> &'static str {
    "fs"
  }
  fn sub_commands(&self) -> &'static [SubCommand] {
    const SUB_COMMANDS: &[SubCommand] = &[
      sub_command!(
        &rename::Rename,
        "rename", "<from> <to>",
        "rename a file within one filesystem, atomically"
      ),
      sub_command!(
        &rmdir::RmDir,
        "rmdir", "<dir> ...",
        "remove empty directories"
      ),
      sub_command!(
        &unlink::Unlink,
        "unlink", "<file> ...",
        "remove files"
      ),
      sub_command!(
        &link::Link,
        "link", "<target> <link> ...",
        "create hard links to files"
      ),
      sub_command!(
        &symlink::SymLink,
        "symlink", "<target> <link> ...",
        "create symbolic links to files"
      ),
      sub_command!(
        &readlink::ReadLink,
        "readlink", "<link>",
        "print the value of a symbolic link"
      ),
      sub_command!(
        &realpath::RealPath,
        "realpath", "<file> ...",
        "print the canonicalized absolute pathname"
      ),
      sub_command!(
        &truncate::Truncate,
        "truncate", "<size> <file> ...",
        "truncate files to a specified size"
      ),
      sub_command!(
        &chown::ChOwn,
        "chown", "<owner>[:<group>] <file> ...",
        "change the owner and/or group of files"
      ),
      sub_command!(
        &chmod::ChMod,
        "chmod", "<mode> <file> ...",
        "change the permissions of files"
      ),
      sub_command!(
        &touch::Touch,
        "touch", "<file> ...",
        "update the access and modification times of files"
      ),
      sub_command!(
        &mkdir::MkDir,
        "mkdir", "<dir> ...",
        "create directories"
      ),
    ];
    SUB_COMMANDS
  }
}
