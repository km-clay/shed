pub struct LineBuf {
	buffer: String
}

impl LineBuf {
	pub fn as_str(&self) -> &str {
		&self.buffer // FIXME: this will have to be fixed up later
	}
}
