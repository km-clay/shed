pub mod echo;
pub mod cd;
pub mod pwd;
pub mod export;
pub mod jobctl;
pub mod read;
pub mod alias;

pub const BUILTINS: [&str;9] = [
	"echo",
	"cd",
	"pwd",
	"export",
	"fg",
	"bg",
	"jobs",
	"read",
	"alias"
];
