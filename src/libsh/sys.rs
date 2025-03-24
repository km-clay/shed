use crate::{prelude::*, state::write_jobs};

pub fn sh_quit(code: i32) -> ! {
	write_jobs(|j| {
		for job in j.jobs_mut().iter_mut().flatten() {
			job.killpg(Signal::SIGTERM).ok();
		}
	});
	if let Some(termios) = unsafe { crate::get_saved_termios() } {
		termios::tcsetattr(std::io::stdin(), termios::SetArg::TCSANOW, &termios).unwrap();
	}
	if code == 0 {
		eprintln!("exit");
	} else {
		eprintln!("exit {code}");
	}
	exit(code);
}
