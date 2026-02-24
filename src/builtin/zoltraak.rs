use std::{os::unix::fs::OpenOptionsExt, sync::LazyLock};

use crate::{
  getopt::{get_opts_from_tokens, Opt, OptSet, OptSpec},
  jobs::JobBldr,
  libsh::error::{Note, ShErr, ShErrKind, ShResult, ShResultExt},
  parse::{NdRule, Node},
  prelude::*,
  procio::{borrow_fd, IoStack},
};

use super::setup_builtin;

bitflags! {
  #[derive(Clone,Copy,Debug,PartialEq,Eq)]
  struct ZoltFlags: u32 {
    const DRY              = 0b000001;
    const CONFIRM          = 0b000010;
    const NO_PRESERVE_ROOT = 0b000100;
    const RECURSIVE        = 0b001000;
    const FORCE            = 0b010000;
    const VERBOSE          = 0b100000;
  }
}

/// Annihilate a file
///
/// This command works similarly to 'rm', but behaves more destructively.
/// The file given as an argument is completely destroyed. The command works by
/// shredding all of the data contained in the file, before truncating the
/// length of the file to 0 to ensure that not even any metadata remains.
pub fn zoltraak(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };
  let zolt_opts = [
    OptSpec {
      opt: Opt::Long("dry-run".into()),
      takes_arg: false,
    },
    OptSpec {
      opt: Opt::Long("confirm".into()),
      takes_arg: false,
    },
    OptSpec {
      opt: Opt::Long("no-preserve-root".into()),
      takes_arg: false,
    },
    OptSpec {
      opt: Opt::Short('r'),
      takes_arg: false,
    },
    OptSpec {
      opt: Opt::Short('f'),
      takes_arg: false,
    },
    OptSpec {
      opt: Opt::Short('v'),
      takes_arg: false,
    },
  ];
  let mut flags = ZoltFlags::empty();

  let (argv, opts) = get_opts_from_tokens(argv, &zolt_opts);

  for opt in opts {
    match opt {
      Opt::Long(flag) => match flag.as_str() {
        "no-preserve-root" => flags |= ZoltFlags::NO_PRESERVE_ROOT,
        "confirm" => flags |= ZoltFlags::CONFIRM,
        "dry-run" => flags |= ZoltFlags::DRY,
        _ => {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            format!("zoltraak: unrecognized option '{flag}'"),
          ));
        }
      },
      Opt::Short(flag) => match flag {
        'r' => flags |= ZoltFlags::RECURSIVE,
        'f' => flags |= ZoltFlags::FORCE,
        'v' => flags |= ZoltFlags::VERBOSE,
        _ => {
          return Err(ShErr::simple(
            ShErrKind::SyntaxErr,
            format!("zoltraak: unrecognized option '{flag}'"),
          ));
        }
      },
      Opt::LongWithArg(flag, _) => {
        return Err(ShErr::simple(
          ShErrKind::SyntaxErr,
          format!("zoltraak: unrecognized option '{flag}'"),
        ));
      }
      Opt::ShortWithArg(flag, _) => {
        return Err(ShErr::simple(
          ShErrKind::SyntaxErr,
          format!("zoltraak: unrecognized option '{flag}'"),
        ));
      }
    }
  }

  let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

  for (arg, span) in argv {
    if &arg == "/" && !flags.contains(ZoltFlags::NO_PRESERVE_ROOT) {
      return Err(
        ShErr::simple(
          ShErrKind::ExecFail,
          "zoltraak: Attempted to destroy root directory '/'",
        )
        .with_note(
          Note::new("If you really want to do this, you can use the --no-preserve-root flag")
            .with_sub_notes(vec!["Example: 'zoltraak --no-preserve-root /'"]),
        ),
      );
    }
    if let Err(e) = annihilate(&arg, flags).blame(span) {
      return Err(e);
    }
  }

  Ok(())
}

fn annihilate(path: &str, flags: ZoltFlags) -> ShResult<()> {
  let path_buf = PathBuf::from(path);
  let is_recursive = flags.contains(ZoltFlags::RECURSIVE);
  let is_verbose = flags.contains(ZoltFlags::VERBOSE);

  const BLOCK_SIZE: u64 = 4096;

  if !path_buf.exists() {
    return Err(ShErr::simple(
      ShErrKind::ExecFail,
      format!("zoltraak: File '{path}' not found"),
    ));
  }

  if path_buf.is_file() {
    let mut file = OpenOptions::new()
      .write(true)
      .custom_flags(libc::O_DIRECT)
      .open(path_buf)?;

    let meta = file.metadata()?;
    let file_size = meta.len();
    let full_blocks = file_size / BLOCK_SIZE;
    let byte_remainder = file_size % BLOCK_SIZE;

    let full_buf = vec![0; BLOCK_SIZE as usize];
    let remainder_buf = vec![0; byte_remainder as usize];

    for _ in 0..full_blocks {
      file.write_all(&full_buf)?;
    }

    if byte_remainder > 0 {
      file.write_all(&remainder_buf)?;
    }

    file.set_len(0)?;
    mem::drop(file);
    fs::remove_file(path)?;
    if is_verbose {
      let stderr = borrow_fd(STDERR_FILENO);
      write(stderr, format!("shredded file '{path}'\n").as_bytes())?;
    }
  } else if path_buf.is_dir() {
    if is_recursive {
      annihilate_recursive(path, flags)?; // scary
    } else {
      return Err(
        ShErr::simple(
          ShErrKind::ExecFail,
          format!("zoltraak: '{path}' is a directory"),
        )
        .with_note(
          Note::new("Use the '-r' flag to recursively shred directories")
            .with_sub_notes(vec!["Example: 'zoltraak -r directory'"]),
        ),
      );
    }
  }

  Ok(())
}

fn annihilate_recursive(dir: &str, flags: ZoltFlags) -> ShResult<()> {
  let dir_path = PathBuf::from(dir);
  let is_verbose = flags.contains(ZoltFlags::VERBOSE);

  for dir_entry in fs::read_dir(&dir_path)? {
    let entry = dir_entry?.path();
    let file = entry.to_str().unwrap();

    if entry.is_file() {
      annihilate(file, flags)?;
    } else if entry.is_dir() {
      annihilate_recursive(file, flags)?;
    }
  }
  fs::remove_dir(dir)?;
  if is_verbose {
    let stderr = borrow_fd(STDERR_FILENO);
    write(stderr, format!("shredded directory '{dir}'\n").as_bytes())?;
  }
  Ok(())
}
