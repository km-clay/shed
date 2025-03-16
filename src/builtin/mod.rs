pub mod echo;
pub mod cd;
pub mod export;
pub mod pwd;
pub mod source;
pub mod shift;
pub mod jobctl;

pub const BUILTINS: [&str;9] = [
	"echo",
	"cd",
	"export",
	"pwd",
	"source",
	"shift",
	"jobs",
	"fg",
	"bg"
];
