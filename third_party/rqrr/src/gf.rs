//! Table-light GF(2^m) arithmetic for the Reed-Solomon decoder.
//!
//! GeekBridge patch: upstream generates the fields with the `g2p` macro, which embeds a 64 KiB multiplication table
//! (random-looking, so it does not compress) for GF(256). This module computes the same fields from 256-byte
//! log/antilog tables built at compile time. Results are identical to `g2p` (generator 2, same moduli).
use core::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

pub trait GaloisField:
    Add<Output = Self>
    + AddAssign
    + Sub<Output = Self>
    + SubAssign
    + Neg<Output = Self>
    + Mul<Output = Self>
    + MulAssign
    + Div<Output = Self>
    + DivAssign
    + Copy
    + PartialEq
    + Eq
{
    const ZERO: Self;
    const ONE: Self;
    const GENERATOR: Self;

    /// `self` to the power `p` (square and multiply; `x.pow(0)` is `ONE`).
    fn pow(self, p: usize) -> Self {
        let mut val = Self::ONE;
        let mut pow_pos = 1usize << (usize::BITS - 1);
        while pow_pos > 0 {
            val *= val;
            if (pow_pos & p) > 0 {
                val *= self;
            }
            pow_pos >>= 1;
        }
        val
    }
}

macro_rules! gf {
    ($name:ident, $bits:expr, $modulus:expr) => {
        #[derive(Clone, Copy, Eq, PartialEq, Debug)]
        pub struct $name(pub u8);

        impl $name {
            const MASK: u8 = ((1u16 << $bits) - 1) as u8;
            const ORDER: usize = (1usize << $bits) - 1;
            /// (antilog table doubled so a sum of two logs needs no reduction, log table)
            const TABLES: ([u8; 2 * ((1usize << $bits) - 1)], [u8; 1usize << $bits]) = {
                let mut exp = [0u8; 2 * ((1usize << $bits) - 1)];
                let mut log = [0u8; 1usize << $bits];
                let mut x: u16 = 1;
                let mut i = 0;
                while i < Self::ORDER {
                    exp[i] = x as u8;
                    exp[i + Self::ORDER] = x as u8;
                    log[x as usize] = i as u8;
                    x <<= 1;
                    if x >> $bits != 0 {
                        x ^= $modulus;
                    }
                    i += 1;
                }
                (exp, log)
            };
            // Sums of logs stay below 2 * ORDER, so one doubled table is enough. Statics keep one copy in memory.
            const EXP: &'static [u8] = &Self::TABLES.0;
            const LOG: &'static [u8] = &Self::TABLES.1;
        }

        impl GaloisField for $name {
            const ZERO: Self = Self(0);
            const ONE: Self = Self(1);
            const GENERATOR: Self = Self(2);
        }

        impl Add for $name {
            type Output = Self;
            fn add(self, rhs: Self) -> Self {
                Self(self.0 ^ rhs.0)
            }
        }
        impl AddAssign for $name {
            fn add_assign(&mut self, rhs: Self) {
                self.0 ^= rhs.0;
            }
        }
        impl Sub for $name {
            type Output = Self;
            fn sub(self, rhs: Self) -> Self {
                Self(self.0 ^ rhs.0)
            }
        }
        impl SubAssign for $name {
            fn sub_assign(&mut self, rhs: Self) {
                self.0 ^= rhs.0;
            }
        }
        impl Neg for $name {
            type Output = Self;
            fn neg(self) -> Self {
                self
            }
        }
        impl Mul for $name {
            type Output = Self;
            fn mul(self, rhs: Self) -> Self {
                let (a, b) = (self.0 & Self::MASK, rhs.0 & Self::MASK);
                if a == 0 || b == 0 {
                    return Self(0);
                }
                Self(Self::EXP[Self::LOG[a as usize] as usize + Self::LOG[b as usize] as usize])
            }
        }
        impl MulAssign for $name {
            fn mul_assign(&mut self, rhs: Self) {
                *self = *self * rhs;
            }
        }
        impl Div for $name {
            type Output = Self;
            fn div(self, rhs: Self) -> Self {
                let (a, b) = (self.0 & Self::MASK, rhs.0 & Self::MASK);
                if b == 0 {
                    panic!(concat!("Division by 0 in ", stringify!($name)));
                }
                if a == 0 {
                    return Self(0);
                }
                Self(Self::EXP[Self::LOG[a as usize] as usize + Self::ORDER - Self::LOG[b as usize] as usize])
            }
        }
        impl DivAssign for $name {
            fn div_assign(&mut self, rhs: Self) {
                *self = *self / rhs;
            }
        }
    };
}

gf!(GF16, 4, 0b1_0011u16);
gf!(GF256, 8, 0b1_0001_1101u16);
