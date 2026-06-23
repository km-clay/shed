use std::cmp::Ordering;

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq)]
pub struct Pos {
  pub row: usize,
  pub col: usize,
}

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq)]
pub struct SignedPos {
  pub row: isize,
  pub col: isize,
}

impl Pos {
  pub const MAX: Self = Pos {
    row: usize::MAX,
    col: usize::MAX,
  };
  pub const MIN: Self = Pos {
    row: usize::MIN,
    col: usize::MIN,
  };

  pub fn new(row: usize, col: usize) -> Self {
    Self { row, col }
  }

  pub fn difference(&self, other: &Pos) -> SignedPos {
    SignedPos {
      row: self.row as isize - other.row as isize,
      col: self.col as isize - other.col as isize,
    }
  }

  pub fn add_signed(&self, other: SignedPos) -> Self {
    Self {
      row: self.row.saturating_add_signed(other.row),
      col: self.col.saturating_add_signed(other.col),
    }
  }

  pub fn row_col_add(&self, row: isize, col: isize) -> Self {
    Self {
      row: self.row.saturating_add_signed(row),
      col: self.col.saturating_add_signed(col),
    }
  }

  pub fn set(&mut self, row: usize, col: usize) {
    self.row = row;
    self.col = col;
  }

  pub fn col_add(&self, rhs: usize) -> Self {
    self.row_col_add(0, rhs as isize)
  }

  pub fn col_add_signed(&self, rhs: isize) -> Self {
    self.row_col_add(0, rhs)
  }

  pub fn col_sub(&self, rhs: usize) -> Self {
    self.row_col_add(0, -(rhs as isize))
  }

  pub fn clamp_row<T>(&mut self, other: &[T]) {
    self.row = self.row.clamp(0, other.len().saturating_sub(1));
  }

  pub fn clamp_col<T>(&mut self, other: &[T], exclusive: bool) {
    let mut max = other.len();
    if exclusive && max > 0 {
      max = max.saturating_sub(1);
    }
    self.col = self.col.clamp(0, max);
  }
}

impl PartialOrd for Pos {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for Pos {
  fn cmp(&self, other: &Self) -> Ordering {
    match self.row.cmp(&other.row) {
      Ordering::Greater => Ordering::Greater,
      Ordering::Less => Ordering::Less,
      Ordering::Equal => self.col.cmp(&other.col),
    }
  }
}

impl std::ops::Add for Pos {
  type Output = Self;

  fn add(self, rhs: Self) -> Self::Output {
    Self {
      row: self.row.saturating_add(rhs.row),
      col: self.col.saturating_add(rhs.col),
    }
  }
}
