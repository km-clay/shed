use std::sync::Mutex;

pub static REGISTERS: Mutex<Registers> = Mutex::new(Registers::new());

pub fn read_register(ch: Option<char>) -> Option<String> {
	let lock = REGISTERS.lock().unwrap();
	lock.get_reg(ch).map(|r| r.buf().clone())
}

pub fn write_register(ch: Option<char>, buf: String) {
	let mut lock = REGISTERS.lock().unwrap();
	if let Some(r) = lock.get_reg_mut(ch) { r.write(buf) }
}

pub fn append_register(ch: Option<char>, buf: String) {
	let mut lock = REGISTERS.lock().unwrap();
	if let Some(r) = lock.get_reg_mut(ch) { r.append(buf) }
}

#[derive(Default,Debug)]
pub struct Registers {
	default: Register,
	a: Register,
	b: Register,
	c: Register,
	d: Register,
	e: Register,
	f: Register,
	g: Register,
	h: Register,
	i: Register,
	j: Register,
	k: Register,
	l: Register,
	m: Register,
	n: Register,
	o: Register,
	p: Register,
	q: Register,
	r: Register,
	s: Register,
	t: Register,
	u: Register,
	v: Register,
	w: Register,
	x: Register,
	y: Register,
	z: Register,
}

impl Registers {
	pub const fn new() -> Self {
		Self {
			default: Register(String::new()),
			a: Register(String::new()),
			b: Register(String::new()),
			c: Register(String::new()),
			d: Register(String::new()),
			e: Register(String::new()),
			f: Register(String::new()),
			g: Register(String::new()),
			h: Register(String::new()),
			i: Register(String::new()),
			j: Register(String::new()),
			k: Register(String::new()),
			l: Register(String::new()),
			m: Register(String::new()),
			n: Register(String::new()),
			o: Register(String::new()),
			p: Register(String::new()),
			q: Register(String::new()),
			r: Register(String::new()),
			s: Register(String::new()),
			t: Register(String::new()),
			u: Register(String::new()),
			v: Register(String::new()),
			w: Register(String::new()),
			x: Register(String::new()),
			y: Register(String::new()),
			z: Register(String::new()),
		}
	}
	pub fn get_reg(&self, ch: Option<char>) -> Option<&Register> {
		let Some(ch) = ch else {
			return Some(&self.default)
		};
		match ch {
			'a' => Some(&self.a),
			'b' => Some(&self.b),
			'c' => Some(&self.c),
			'd' => Some(&self.d),
			'e' => Some(&self.e),
			'f' => Some(&self.f),
			'g' => Some(&self.g),
			'h' => Some(&self.h),
			'i' => Some(&self.i),
			'j' => Some(&self.j),
			'k' => Some(&self.k),
			'l' => Some(&self.l),
			'm' => Some(&self.m),
			'n' => Some(&self.n),
			'o' => Some(&self.o),
			'p' => Some(&self.p),
			'q' => Some(&self.q),
			'r' => Some(&self.r),
			's' => Some(&self.s),
			't' => Some(&self.t),
			'u' => Some(&self.u),
			'v' => Some(&self.v),
			'w' => Some(&self.w),
			'x' => Some(&self.x),
			'y' => Some(&self.y),
			'z' => Some(&self.z),
			_ => None
		}
	}
	pub fn get_reg_mut(&mut self, ch: Option<char>) -> Option<&mut Register> {
		let Some(ch) = ch else {
			return Some(&mut self.default)
		};
		match ch {
			'a' => Some(&mut self.a),
			'b' => Some(&mut self.b),
			'c' => Some(&mut self.c),
			'd' => Some(&mut self.d),
			'e' => Some(&mut self.e),
			'f' => Some(&mut self.f),
			'g' => Some(&mut self.g),
			'h' => Some(&mut self.h),
			'i' => Some(&mut self.i),
			'j' => Some(&mut self.j),
			'k' => Some(&mut self.k),
			'l' => Some(&mut self.l),
			'm' => Some(&mut self.m),
			'n' => Some(&mut self.n),
			'o' => Some(&mut self.o),
			'p' => Some(&mut self.p),
			'q' => Some(&mut self.q),
			'r' => Some(&mut self.r),
			's' => Some(&mut self.s),
			't' => Some(&mut self.t),
			'u' => Some(&mut self.u),
			'v' => Some(&mut self.v),
			'w' => Some(&mut self.w),
			'x' => Some(&mut self.x),
			'y' => Some(&mut self.y),
			'z' => Some(&mut self.z),
			_ => None
		}
	}
}

#[derive(Clone,Default,Debug)]
pub struct Register(String);
impl Register {
	pub fn buf(&self) -> &String {
		&self.0
	}
	pub fn write(&mut self, buf: String) {
		self.0 = buf
	}
	pub fn append(&mut self, buf: String) {
		self.0.push_str(&buf)
	}
	pub fn clear(&mut self) {
		self.0.clear()
	}
}
