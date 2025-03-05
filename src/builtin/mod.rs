pub mod echo;
pub mod cd;
pub mod pwd;
pub mod export;
pub mod jobctl;
pub mod read;
pub mod alias;
pub mod control_flow;

pub const BUILTINS: [&str;13] = [
	"echo",
	"cd",
	"pwd",
	"export",
	"fg",
	"bg",
	"jobs",
	"read",
	"alias",
	"exit",
	"continue",
	"return",
	"break",
];
