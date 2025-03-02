pub mod echo;
pub mod cd;
pub mod pwd;
pub mod export;
pub mod jobctl;
pub mod read;

pub const BUILTINS: [&str;8] = [
	"echo",
	"cd",
	"pwd",
	"export",
	"fg",
	"bg",
	"jobs",
	"read"
];
