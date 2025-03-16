pub mod echo;
pub mod cd;
pub mod export;
pub mod pwd;
pub mod source;
pub mod shift;

pub const BUILTINS: [&str;6] = [
	"echo",
	"cd",
	"export",
	"pwd",
	"source",
	"shift"
];
