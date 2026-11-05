// The following is adapted from fdlibm (http://www.netlib.org/fdlibm).
//
// ====================================================
// Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.
//
// Developed at SunSoft, a Sun Microsystems, Inc. business.
// Permission to use, copy, modify, and distribute this
// software is freely granted, provided that this notice
// is preserved.
// ====================================================
//
// The original source code covered by the above license above has been
// modified significantly by Google Inc.
// Copyright 2016 the V8 project authors. All rights reserved.
//
// Ported from V8 12.4.254 src/base/ieee754.cc (Node 22 matches it bit for bit: tools/js/math_check.sh).
// Source: https://github.com/v8/v8/blob/12.4.254/src/base/ieee754.cc
// See ../LICENSE-V8 for the BSD-3-Clause terms.

// The port keeps the C shape (late initialization, `x - x` for NaN) so it can be read against the source.
#![allow(non_upper_case_globals, non_snake_case, clippy::excessive_precision, clippy::approx_constant, clippy::needless_late_init, clippy::eq_op)]

// Word access helpers (the EXTRACT_WORDS family of macros).

#[inline(always)]
fn high_word(d: f64) -> i32 {
    (d.to_bits() >> 32) as i32
}

#[inline(always)]
fn low_word(d: f64) -> u32 {
    d.to_bits() as u32
}

#[inline(always)]
fn insert_words(ix0: i32, ix1: u32) -> f64 {
    f64::from_bits(((ix0 as u32 as u64) << 32) | ix1 as u64)
}

#[inline(always)]
fn set_high_word(d: f64, v: i32) -> f64 {
    f64::from_bits((d.to_bits() & 0x0000_0000_FFFF_FFFF) | ((v as u32 as u64) << 32))
}

#[inline(always)]
fn set_low_word(d: f64, v: u32) -> f64 {
    f64::from_bits((d.to_bits() & 0xFFFF_FFFF_0000_0000) | v as u64)
}

/* __ieee754_rem_pio2(x,y)
 *
 * return the remainder of x rem pi/2 in y[0]+y[1]
 * use __kernel_rem_pio2()
 */
fn rem_pio2(x: f64, y: &mut [f64; 2]) -> i32 {
    /*
     * Table of constants for 2/pi, 396 Hex digits (476 decimal) of 2/pi
     */
    static two_over_pi: [i32; 66] = [
        0xA2F983, 0x6E4E44, 0x1529FC, 0x2757D1, 0xF534DD, 0xC0DB62, 0x95993C, 0x439041, 0xFE5163,
        0xABDEBB, 0xC561B7, 0x246E3A, 0x424DD2, 0xE00649, 0x2EEA09, 0xD1921C, 0xFE1DEB, 0x1CB129,
        0xA73EE8, 0x8235F5, 0x2EBB44, 0x84E99C, 0x7026B4, 0x5F7E41, 0x3991D6, 0x398353, 0x39F49C,
        0x845F8B, 0xBDF928, 0x3B1FF8, 0x97FFDE, 0x05980F, 0xEF2F11, 0x8B5A0A, 0x6D1F6D, 0x367ECF,
        0x27CB09, 0xB74F46, 0x3F669E, 0x5FEA2D, 0x7527BA, 0xC7EBE5, 0xF17B3D, 0x0739F7, 0x8A5292,
        0xEA6BFB, 0x5FB11F, 0x8D5D08, 0x560330, 0x46FC7B, 0x6BABF0, 0xCFBC20, 0x9AF436, 0x1DA9E3,
        0x91615E, 0xE61B08, 0x659985, 0x5F14A0, 0x68408D, 0xFFD880, 0x4D7327, 0x310606, 0x1556CA,
        0x73A8C9, 0x60E27B, 0xC08C6B,
    ];

    static npio2_hw: [i32; 32] = [
        0x3FF921FB, 0x400921FB, 0x4012D97C, 0x401921FB, 0x401F6A7A, 0x4022D97C, 0x4025FDBB,
        0x402921FB, 0x402C463A, 0x402F6A7A, 0x4031475C, 0x4032D97C, 0x40346B9C, 0x4035FDBB,
        0x40378FDB, 0x403921FB, 0x403AB41B, 0x403C463A, 0x403DD85A, 0x403F6A7A, 0x40407E4C,
        0x4041475C, 0x4042106C, 0x4042D97C, 0x4043A28C, 0x40446B9C, 0x404534AC, 0x4045FDBB,
        0x4046C6CB, 0x40478FDB, 0x404858EB, 0x404921FB,
    ];

    /*
     * invpio2:  53 bits of 2/pi
     * pio2_1:   first  33 bit of pi/2
     * pio2_1t:  pi/2 - pio2_1
     * pio2_2:   second 33 bit of pi/2
     * pio2_2t:  pi/2 - (pio2_1+pio2_2)
     * pio2_3:   third  33 bit of pi/2
     * pio2_3t:  pi/2 - (pio2_1+pio2_2+pio2_3)
     */

    const zero: f64 = 0.00000000000000000000e+00; /* 0x00000000, 0x00000000 */
    const half: f64 = 5.00000000000000000000e-01; /* 0x3FE00000, 0x00000000 */
    const two24: f64 = 1.67772160000000000000e+07; /* 0x41700000, 0x00000000 */
    const invpio2: f64 = 6.36619772367581382433e-01; /* 0x3FE45F30, 0x6DC9C883 */
    const pio2_1: f64 = 1.57079632673412561417e+00; /* 0x3FF921FB, 0x54400000 */
    const pio2_1t: f64 = 6.07710050650619224932e-11; /* 0x3DD0B461, 0x1A626331 */
    const pio2_2: f64 = 6.07710050630396597660e-11; /* 0x3DD0B461, 0x1A600000 */
    const pio2_2t: f64 = 2.02226624879595063154e-21; /* 0x3BA3198A, 0x2E037073 */
    const pio2_3: f64 = 2.02226624871116645580e-21; /* 0x3BA3198A, 0x2E000000 */
    const pio2_3t: f64 = 8.47842766036889956997e-32; /* 0x397B839A, 0x252049C1 */

    let mut z: f64;
    let mut w: f64;
    let mut t: f64;
    let mut r: f64;
    let fn_: f64;
    let mut tx = [0.0f64; 3];
    let hx: i32;
    let ix: i32;

    hx = high_word(x); /* high word of x */
    ix = hx & 0x7FFFFFFF;
    if ix <= 0x3FE921FB {
        /* |x| ~<= pi/4 , no need for reduction */
        y[0] = x;
        y[1] = 0.0;
        return 0;
    }
    if ix < 0x4002D97C {
        /* |x| < 3pi/4, special case with n=+-1 */
        if hx > 0 {
            z = x - pio2_1;
            if ix != 0x3FF921FB {
                /* 33+53 bit pi is good enough */
                y[0] = z - pio2_1t;
                y[1] = (z - y[0]) - pio2_1t;
            } else {
                /* near pi/2, use 33+33+53 bit pi */
                z -= pio2_2;
                y[0] = z - pio2_2t;
                y[1] = (z - y[0]) - pio2_2t;
            }
            return 1;
        } else {
            /* negative x */
            z = x + pio2_1;
            if ix != 0x3FF921FB {
                /* 33+53 bit pi is good enough */
                y[0] = z + pio2_1t;
                y[1] = (z - y[0]) + pio2_1t;
            } else {
                /* near pi/2, use 33+33+53 bit pi */
                z += pio2_2;
                y[0] = z + pio2_2t;
                y[1] = (z - y[0]) + pio2_2t;
            }
            return -1;
        }
    }
    if ix <= 0x413921FB {
        /* |x| ~<= 2^19*(pi/2), medium size */
        t = x.abs();
        let n = (t * invpio2 + half) as i32;
        fn_ = n as f64;
        r = t - fn_ * pio2_1;
        w = fn_ * pio2_1t; /* 1st round good to 85 bit */
        if n < 32 && ix != npio2_hw[(n - 1) as usize] {
            y[0] = r - w; /* quick check no cancellation */
        } else {
            let mut high: u32;
            let j = ix >> 20;
            y[0] = r - w;
            high = high_word(y[0]) as u32;
            let mut i = j - ((high >> 20) & 0x7FF) as i32;
            if i > 16 {
                /* 2nd iteration needed, good to 118 */
                t = r;
                w = fn_ * pio2_2;
                r = t - w;
                w = fn_ * pio2_2t - ((t - r) - w);
                y[0] = r - w;
                high = high_word(y[0]) as u32;
                i = j - ((high >> 20) & 0x7FF) as i32;
                if i > 49 {
                    /* 3rd iteration need, 151 bits acc */
                    t = r; /* will cover all possible cases */
                    w = fn_ * pio2_3;
                    r = t - w;
                    w = fn_ * pio2_3t - ((t - r) - w);
                    y[0] = r - w;
                }
            }
        }
        y[1] = (r - y[0]) - w;
        if hx < 0 {
            y[0] = -y[0];
            y[1] = -y[1];
            return -n;
        } else {
            return n;
        }
    }
    /*
     * all other (large) arguments
     */
    if ix >= 0x7FF00000 {
        /* x is inf or NaN */
        y[0] = x - x;
        y[1] = y[0];
        return 0;
    }
    /* set z = scalbn(|x|,ilogb(x)-23) */
    let low = low_word(x);
    z = set_low_word(0.0, low);
    let e0 = (ix >> 20) - 1046; /* e0 = ilogb(z)-23; */
    z = set_high_word(z, ix.wrapping_sub(((e0 as u32) << 20) as i32));
    for t in tx.iter_mut().take(2) {
        *t = (z as i32) as f64;
        z = (z - *t) * two24;
    }
    tx[2] = z;
    let mut nx: i32 = 3;
    while tx[(nx - 1) as usize] == zero {
        nx -= 1; /* skip zero term */
    }
    let n = kernel_rem_pio2(&tx, y, e0, nx, 2, &two_over_pi);
    if hx < 0 {
        y[0] = -y[0];
        y[1] = -y[1];
        return -n;
    }
    n
}

/* __kernel_cos( x,  y )
 * kernel cos function on [-pi/4, pi/4], pi/4 ~ 0.785398164
 * Input x is assumed to be bounded by ~pi/4 in magnitude.
 * Input y is the tail of x.
 *
 * Algorithm
 *      1. Since cos(-x) = cos(x), we need only to consider positive x.
 *      2. if x < 2^-27 (hx<0x3E400000 0), return 1 with inexact if x!=0.
 *      3. cos(x) is approximated by a polynomial of degree 14 on
 *         [0,pi/4]
 *                                       4            14
 *              cos(x) ~ 1 - x*x/2 + C1*x + ... + C6*x
 *      4. let r = C1*x +C2*x +C3*x +C4*x +C5*x  +C6*x  , then
 *             cos(x) = 1 - x*x/2 + r
 *         since cos(x+y) ~ cos(x) - sin(x)*y
 *                        ~ cos(x) - x*y,
 *         a correction term is necessary in cos(x) and hence
 *              cos(x+y) = 1 - (x*x/2 - (r - x*y))
 *         For better accuracy when x > 0.3, let qx = |x|/4 with
 *         the last 32 bits mask off, and if x > 0.78125, let qx = 0.28125.
 *         Then
 *              cos(x+y) = (1-qx) - ((x*x/2-qx) - (r-x*y)).
 */
fn kernel_cos(x: f64, y: f64) -> f64 {
    const one: f64 = 1.00000000000000000000e+00; /* 0x3FF00000, 0x00000000 */
    const C1: f64 = 4.16666666666666019037e-02; /* 0x3FA55555, 0x5555554C */
    const C2: f64 = -1.38888888888741095749e-03; /* 0xBF56C16C, 0x16C15177 */
    const C3: f64 = 2.48015872894767294178e-05; /* 0x3EFA01A0, 0x19CB1590 */
    const C4: f64 = -2.75573143513906633035e-07; /* 0xBE927E4F, 0x809C52AD */
    const C5: f64 = 2.08757232129817482790e-09; /* 0x3E21EE9E, 0xBDB4B1C4 */
    const C6: f64 = -1.13596475577881948265e-11; /* 0xBDA8FAE9, 0xBE8838D4 */

    let a: f64;
    let iz: f64;
    let z: f64;
    let r: f64;
    let qx: f64;
    let mut ix: i32;
    ix = high_word(x);
    ix &= 0x7FFFFFFF; /* ix = |x|'s high word*/
    if ix < 0x3E400000 {
        /* if x < 2**27 */
        if (x as i32) == 0 {
            return one; /* generate inexact */
        }
    }
    z = x * x;
    r = z * (C1 + z * (C2 + z * (C3 + z * (C4 + z * (C5 + z * C6)))));
    if ix < 0x3FD33333 {
        /* if |x| < 0.3 */
        one - (0.5 * z - (z * r - x * y))
    } else {
        if ix > 0x3FE90000 {
            /* x > 0.78125 */
            qx = 0.28125;
        } else {
            qx = insert_words(ix - 0x00200000, 0); /* x/4 */
        }
        iz = 0.5 * z - qx;
        a = one - qx;
        a - (iz - (z * r - x * y))
    }
}

/* __kernel_rem_pio2(x,y,e0,nx,prec,ipio2)
 * double x[],y[]; int e0,nx,prec; int ipio2[];
 *
 * __kernel_rem_pio2 return the last three digits of N with
 *              y = x - N*pi/2
 * so that |y| < pi/2.
 *
 * (See the long description in V8's ieee754.cc; the code below follows it.)
 */
fn kernel_rem_pio2(
    x: &[f64],
    y: &mut [f64],
    e0: i32,
    nx: i32,
    prec: usize,
    ipio2: &[i32],
) -> i32 {
    /* Constants:
     * The hexadecimal values are the intended ones for the following
     * constants. The decimal values may be used, provided that the
     * compiler will convert from decimal to binary accurately enough
     * to produce the hexadecimal values shown.
     */
    static init_jk: [i32; 4] = [2, 3, 4, 6]; /* initial value for jk */

    static PIo2: [f64; 8] = [
        1.57079625129699707031e+00, /* 0x3FF921FB, 0x40000000 */
        7.54978941586159635335e-08, /* 0x3E74442D, 0x00000000 */
        5.39030252995776476554e-15, /* 0x3CF84698, 0x80000000 */
        3.28200341580791294123e-22, /* 0x3B78CC51, 0x60000000 */
        1.27065575308067607349e-29, /* 0x39F01B83, 0x80000000 */
        1.22933308981111328932e-36, /* 0x387A2520, 0x40000000 */
        2.73370053816464559624e-44, /* 0x36E38222, 0x80000000 */
        2.16741683877804819444e-51, /* 0x3569F31D, 0x00000000 */
    ];

    const zero: f64 = 0.0;
    const one: f64 = 1.0;
    const two24: f64 = 1.67772160000000000000e+07; /* 0x41700000, 0x00000000 */
    const twon24: f64 = 5.96046447753906250000e-08; /* 0x3E700000, 0x00000000 */

    let mut iq = [0i32; 20];
    let mut f = [0f64; 20];
    let mut fq = [0f64; 20];
    let mut q = [0f64; 20];
    let mut z: f64;
    let mut fw: f64;
    let mut i: i32;
    let mut j: i32;
    let mut k: i32;
    let n: i32;
    let mut ih: i32;
    let mut carry: i32;

    /* initialize jk*/
    let jk = init_jk[prec];
    let jp = jk;

    /* determine jx,jv,q0, note that 3>q0 */
    let jx = nx - 1;
    let mut jv = (e0 - 3) / 24;
    if jv < 0 {
        jv = 0;
    }
    let mut q0 = e0 - 24 * (jv + 1);

    /* set up f[0] to f[jx+jk] where f[jx+jk] = ipio2[jv+jk] */
    j = jv - jx;
    let m = jx + jk;
    i = 0;
    while i <= m {
        f[i as usize] = if j < 0 { zero } else { ipio2[j as usize] as f64 };
        i += 1;
        j += 1;
    }

    /* compute q[0],q[1],...q[jk] */
    i = 0;
    while i <= jk {
        j = 0;
        fw = 0.0;
        while j <= jx {
            fw += x[j as usize] * f[(jx + i - j) as usize];
            j += 1;
        }
        q[i as usize] = fw;
        i += 1;
    }

    let mut jz = jk;
    'recompute: loop {
        /* distill q[] into iq[] reversingly */
        i = 0;
        j = jz;
        z = q[jz as usize];
        while j > 0 {
            fw = ((twon24 * z) as i32) as f64;
            iq[i as usize] = (z - two24 * fw) as i32;
            z = q[(j - 1) as usize] + fw;
            i += 1;
            j -= 1;
        }

        /* compute n */
        z = scalbn(z, q0); /* actual value of z */
        z -= 8.0 * (z * 0.125).floor(); /* trim off integer >= 8 */
        let mut nn = z as i32;
        z -= nn as f64;
        ih = 0;
        if q0 > 0 {
            /* need iq[jz-1] to determine n */
            i = iq[(jz - 1) as usize] >> (24 - q0);
            nn += i;
            iq[(jz - 1) as usize] -= i << (24 - q0);
            ih = iq[(jz - 1) as usize] >> (23 - q0);
        } else if q0 == 0 {
            ih = iq[(jz - 1) as usize] >> 23;
        } else if z >= 0.5 {
            ih = 2;
        }

        if ih > 0 {
            /* q > 0.5 */
            nn += 1;
            carry = 0;
            i = 0;
            while i < jz {
                /* compute 1-q */
                j = iq[i as usize];
                if carry == 0 {
                    if j != 0 {
                        carry = 1;
                        iq[i as usize] = 0x1000000 - j;
                    }
                } else {
                    iq[i as usize] = 0xFFFFFF - j;
                }
                i += 1;
            }
            if q0 > 0 {
                /* rare case: chance is 1 in 12 */
                match q0 {
                    1 => iq[(jz - 1) as usize] &= 0x7FFFFF,
                    2 => iq[(jz - 1) as usize] &= 0x3FFFFF,
                    _ => {}
                }
            }
            if ih == 2 {
                z = one - z;
                if carry != 0 {
                    z -= scalbn(one, q0);
                }
            }
        }

        /* check if recomputation is needed */
        if z == zero {
            j = 0;
            i = jz - 1;
            while i >= jk {
                j |= iq[i as usize];
                i -= 1;
            }
            if j == 0 {
                /* need recomputation */
                k = 1;
                while jk >= k && iq[(jk - k) as usize] == 0 {
                    /* k = no. of terms needed */
                    k += 1;
                }

                i = jz + 1;
                while i <= jz + k {
                    /* add q[jz+1] to q[jz+k] */
                    f[(jx + i) as usize] = ipio2[(jv + i) as usize] as f64;
                    j = 0;
                    fw = 0.0;
                    while j <= jx {
                        fw += x[j as usize] * f[(jx + i - j) as usize];
                        j += 1;
                    }
                    q[i as usize] = fw;
                    i += 1;
                }
                jz += k;
                continue 'recompute;
            }
        }
        n = nn;
        break;
    }

    /* chop off zero terms */
    if z == 0.0 {
        jz -= 1;
        q0 -= 24;
        while iq[jz as usize] == 0 {
            jz -= 1;
            q0 -= 24;
        }
    } else {
        /* break z into 24-bit if necessary */
        z = scalbn(z, -q0);
        if z >= two24 {
            fw = ((twon24 * z) as i32) as f64;
            iq[jz as usize] = (z - two24 * fw) as i32;
            jz += 1;
            q0 += 24;
            iq[jz as usize] = fw as i32;
        } else {
            iq[jz as usize] = z as i32;
        }
    }

    /* convert integer "bit" chunk to floating-point value */
    fw = scalbn(one, q0);
    i = jz;
    while i >= 0 {
        q[i as usize] = fw * iq[i as usize] as f64;
        fw *= twon24;
        i -= 1;
    }

    /* compute PIo2[0,...,jp]*q[jz,...,0] */
    i = jz;
    while i >= 0 {
        fw = 0.0;
        k = 0;
        while k <= jp && k <= jz - i {
            fw += PIo2[k as usize] * q[(i + k) as usize];
            k += 1;
        }
        fq[(jz - i) as usize] = fw;
        i -= 1;
    }

    /* compress fq[] into y[] */
    match prec {
        0 => {
            fw = 0.0;
            i = jz;
            while i >= 0 {
                fw += fq[i as usize];
                i -= 1;
            }
            y[0] = if ih == 0 { fw } else { -fw };
        }
        1 | 2 => {
            fw = 0.0;
            i = jz;
            while i >= 0 {
                fw += fq[i as usize];
                i -= 1;
            }
            y[0] = if ih == 0 { fw } else { -fw };
            fw = fq[0] - fw;
            i = 1;
            while i <= jz {
                fw += fq[i as usize];
                i += 1;
            }
            y[1] = if ih == 0 { fw } else { -fw };
        }
        _ => {
            /* case 3: painful */
            i = jz;
            while i > 0 {
                fw = fq[(i - 1) as usize] + fq[i as usize];
                fq[i as usize] += fq[(i - 1) as usize] - fw;
                fq[(i - 1) as usize] = fw;
                i -= 1;
            }
            i = jz;
            while i > 1 {
                fw = fq[(i - 1) as usize] + fq[i as usize];
                fq[i as usize] += fq[(i - 1) as usize] - fw;
                fq[(i - 1) as usize] = fw;
                i -= 1;
            }
            fw = 0.0;
            i = jz;
            while i >= 2 {
                fw += fq[i as usize];
                i -= 1;
            }
            if ih == 0 {
                y[0] = fq[0];
                y[1] = fq[1];
                y[2] = fw;
            } else {
                y[0] = -fq[0];
                y[1] = -fq[1];
                y[2] = -fw;
            }
        }
    }
    n & 7
}

/* __kernel_sin( x, y, iy)
 * kernel sin function on [-pi/4, pi/4], pi/4 ~ 0.7854
 * Input x is assumed to be bounded by ~pi/4 in magnitude.
 * Input y is the tail of x.
 * Input iy indicates whether y is 0. (if iy=0, y assume to be 0).
 *
 *      4. sin(x+y) = sin(x) + sin'(x')*y
 *                  ~ sin(x) + (1-x*x/2)*y
 *         For better accuracy, let
 *                   3      2      2      2      2
 *              r = x *(S2+x *(S3+x *(S4+x *(S5+x *S6))))
 *         then                   3    2
 *              sin(x) = x + (S1*x + (x *(r-y/2)+y))
 */
fn kernel_sin(x: f64, y: f64, iy: i32) -> f64 {
    const half: f64 = 5.00000000000000000000e-01; /* 0x3FE00000, 0x00000000 */
    const S1: f64 = -1.66666666666666324348e-01; /* 0xBFC55555, 0x55555549 */
    const S2: f64 = 8.33333333332248946124e-03; /* 0x3F811111, 0x1110F8A6 */
    const S3: f64 = -1.98412698298579493134e-04; /* 0xBF2A01A0, 0x19C161D5 */
    const S4: f64 = 2.75573137070700676789e-06; /* 0x3EC71DE3, 0x57B1FE7D */
    const S5: f64 = -2.50507602534068634195e-08; /* 0xBE5AE5E6, 0x8A2B9CEB */
    const S6: f64 = 1.58969099521155010221e-10; /* 0x3DE5D93A, 0x5ACFD57C */

    let z: f64;
    let r: f64;
    let v: f64;
    let mut ix: i32;
    ix = high_word(x);
    ix &= 0x7FFFFFFF; /* high word of x */
    if ix < 0x3E400000 {
        /* |x| < 2**-27 */
        if (x as i32) == 0 {
            return x;
        }
    } /* generate inexact */
    z = x * x;
    v = z * x;
    r = S2 + z * (S3 + z * (S4 + z * (S5 + z * S6)));
    if iy == 0 {
        x + v * (S1 + z * r)
    } else {
        x - ((z * (half * y - v * r) - y) - v * S1)
    }
}

/* __kernel_tan( x, y, k )
 * kernel tan function on [-pi/4, pi/4], pi/4 ~ 0.7854
 * Input x is assumed to be bounded by ~pi/4 in magnitude.
 * Input y is the tail of x.
 * Input k indicates whether tan (if k=1) or
 * -1/tan (if k= -1) is returned.
 */
fn kernel_tan(mut x: f64, mut y: f64, iy: i32) -> f64 {
    static xxx: [f64; 16] = [
        3.33333333333334091986e-01,  /* 3FD55555, 55555563 */
        1.33333333333201242699e-01,  /* 3FC11111, 1110FE7A */
        5.39682539762260521377e-02,  /* 3FABA1BA, 1BB341FE */
        2.18694882948595424599e-02,  /* 3F9664F4, 8406D637 */
        8.86323982359930005737e-03,  /* 3F8226E3, E96E8493 */
        3.59207910759131235356e-03,  /* 3F6D6D22, C9560328 */
        1.45620945432529025516e-03,  /* 3F57DBC8, FEE08315 */
        5.88041240820264096874e-04,  /* 3F4344D8, F2F26501 */
        2.46463134818469906812e-04,  /* 3F3026F7, 1A8D1068 */
        7.81794442939557092300e-05,  /* 3F147E88, A03792A6 */
        7.14072491382608190305e-05,  /* 3F12B80F, 32F0A7E9 */
        -1.85586374855275456654e-05, /* BEF375CB, DB605373 */
        2.59073051863633712884e-05,  /* 3EFB2A70, 74BF7AD4 */
        /* one */ 1.00000000000000000000e+00, /* 3FF00000, 00000000 */
        /* pio4 */ 7.85398163397448278999e-01, /* 3FE921FB, 54442D18 */
        /* pio4lo */ 3.06161699786838301793e-17, /* 3C81A626, 33145C07 */
    ];
    let one = xxx[13];
    let pio4 = xxx[14];
    let pio4lo = xxx[15];
    let T = &xxx;

    let mut z: f64;
    let mut r: f64;
    let mut v: f64;
    let mut w: f64;
    let mut s: f64;

    let hx = high_word(x); /* high word of x */
    let ix = hx & 0x7FFFFFFF; /* high word of |x| */
    if ix < 0x3E300000 {
        /* x < 2**-28 */
        if (x as i32) == 0 {
            /* generate inexact */
            let low = low_word(x);
            if (((ix as u32) | low) | ((iy + 1) as u32)) == 0 {
                return one / x.abs();
            } else if iy == 1 {
                return x;
            } else {
                /* compute -1 / (x+y) carefully */
                let a: f64;
                let mut t: f64;

                w = x + y;
                z = w;
                z = set_low_word(z, 0);
                v = y - (z - x);
                a = -one / w;
                t = a;
                t = set_low_word(t, 0);
                s = one + t * z;
                return t + a * (s + t * v);
            }
        }
    }
    if ix >= 0x3FE59428 {
        /* |x| >= 0.6744 */
        if hx < 0 {
            x = -x;
            y = -y;
        }
        z = pio4 - x;
        w = pio4lo - y;
        x = z + w;
        y = 0.0;
    }
    z = x * x;
    w = z * z;
    /*
     * Break x^5*(T[1]+x^2*T[2]+...) into
     * x^5(T[1]+x^4*T[3]+...+x^20*T[11]) +
     * x^5(x^2*(T[2]+x^4*T[4]+...+x^22*[T12]))
     */
    r = T[1] + w * (T[3] + w * (T[5] + w * (T[7] + w * (T[9] + w * T[11]))));
    v = z * (T[2] + w * (T[4] + w * (T[6] + w * (T[8] + w * (T[10] + w * T[12])))));
    s = z * x;
    r = y + z * (s * (r + v) + y);
    r += T[0] * s;
    w = x + r;
    if ix >= 0x3FE59428 {
        v = iy as f64;
        return ((1 - ((hx >> 30) & 2)) as f64) * (v - 2.0 * (x - (w * w / (w + v) - r)));
    }
    if iy == 1 {
        w
    } else {
        /*
         * if allow error up to 2 ulp, simply return
         * -1.0 / (x+r) here
         */
        /* compute -1.0 / (x+r) accurately */
        let a: f64;
        let mut t: f64;
        z = w;
        z = set_low_word(z, 0);
        v = r - (z - x); /* z+v = r+x */
        a = -1.0 / w; /* a = -1.0/w */
        t = a;
        t = set_low_word(t, 0);
        s = 1.0 + t * z;
        t + a * (s + t * v)
    }
}

/* acos(x)
 * Method :
 *      acos(x)  = pi/2 - asin(x)
 *      acos(-x) = pi/2 + asin(x)
 * For |x|<=0.5
 *      acos(x) = pi/2 - (x + x*x^2*R(x^2))     (see asin.c)
 * For x>0.5
 *      acos(x) = pi/2 - (pi/2 - 2asin(sqrt((1-x)/2)))
 *              = 2asin(sqrt((1-x)/2))
 *              = 2s + 2s*z*R(z)        ...z=(1-x)/2, s=sqrt(z)
 *              = 2f + (2c + 2s*z*R(z))
 *     where f=hi part of s, and c = (z-f*f)/(s+f) is the correction term
 *     for f so that f+c ~ sqrt(z).
 * For x<-0.5
 *      acos(x) = pi - 2asin(sqrt((1-|x|)/2))
 *              = pi - 0.5*(s+s*z*R(z)), where z=(1-|x|)/2,s=sqrt(z)
 *
 * Special cases:
 *      if x is NaN, return x itself;
 *      if |x|>1, return NaN with invalid signal.
 *
 * Function needed: sqrt
 */
pub fn acos(x: f64) -> f64 {
    const one: f64 = 1.00000000000000000000e+00; /* 0x3FF00000, 0x00000000 */
    const pi: f64 = 3.14159265358979311600e+00; /* 0x400921FB, 0x54442D18 */
    const pio2_hi: f64 = 1.57079632679489655800e+00; /* 0x3FF921FB, 0x54442D18 */
    const pio2_lo: f64 = 6.12323399573676603587e-17; /* 0x3C91A626, 0x33145C07 */
    const pS0: f64 = 1.66666666666666657415e-01; /* 0x3FC55555, 0x55555555 */
    const pS1: f64 = -3.25565818622400915405e-01; /* 0xBFD4D612, 0x03EB6F7D */
    const pS2: f64 = 2.01212532134862925881e-01; /* 0x3FC9C155, 0x0E884455 */
    const pS3: f64 = -4.00555345006794114027e-02; /* 0xBFA48228, 0xB5688F3B */
    const pS4: f64 = 7.91534994289814532176e-04; /* 0x3F49EFE0, 0x7501B288 */
    const pS5: f64 = 3.47933107596021167570e-05; /* 0x3F023DE1, 0x0DFDF709 */
    const qS1: f64 = -2.40339491173441421878e+00; /* 0xC0033A27, 0x1C8A2D4B */
    const qS2: f64 = 2.02094576023350569471e+00; /* 0x40002AE5, 0x9C598AC8 */
    const qS3: f64 = -6.88283971605453293030e-01; /* 0xBFE6066C, 0x1B8D0159 */
    const qS4: f64 = 7.70381505559019352791e-02; /* 0x3FB3B8C5, 0xB12E9282 */

    let z: f64;
    let p: f64;
    let q: f64;
    let r: f64;
    let w: f64;
    let s: f64;
    let c: f64;
    let mut df: f64;
    let hx = high_word(x);
    let ix = hx & 0x7FFFFFFF;
    if ix >= 0x3FF00000 {
        /* |x| >= 1 */
        let lx = low_word(x);
        if (((ix - 0x3FF00000) as u32) | lx) == 0 {
            /* |x|==1 */
            if hx > 0 {
                return 0.0; /* acos(1) = 0  */
            } else {
                return pi + 2.0 * pio2_lo; /* acos(-1)= pi */
            }
        }
        return f64::NAN; // acos(|x|>1) is NaN
    }
    if ix < 0x3FE00000 {
        /* |x| < 0.5 */
        if ix <= 0x3C600000 {
            return pio2_hi + pio2_lo; /*if|x|<2**-57*/
        }
        z = x * x;
        p = z * (pS0 + z * (pS1 + z * (pS2 + z * (pS3 + z * (pS4 + z * pS5)))));
        q = one + z * (qS1 + z * (qS2 + z * (qS3 + z * qS4)));
        r = p / q;
        pio2_hi - (x - (pio2_lo - x * r))
    } else if hx < 0 {
        /* x < -0.5 */
        z = (one + x) * 0.5;
        p = z * (pS0 + z * (pS1 + z * (pS2 + z * (pS3 + z * (pS4 + z * pS5)))));
        q = one + z * (qS1 + z * (qS2 + z * (qS3 + z * qS4)));
        s = z.sqrt();
        r = p / q;
        w = r * s - pio2_lo;
        pi - 2.0 * (s + w)
    } else {
        /* x > 0.5 */
        z = (one - x) * 0.5;
        s = z.sqrt();
        df = s;
        df = set_low_word(df, 0);
        c = (z - df * df) / (s + df);
        p = z * (pS0 + z * (pS1 + z * (pS2 + z * (pS3 + z * (pS4 + z * pS5)))));
        q = one + z * (qS1 + z * (qS2 + z * (qS3 + z * qS4)));
        r = p / q;
        w = r * s + c;
        2.0 * (df + w)
    }
}

/* acosh(x)
 * Method :
 *      Based on
 *              acosh(x) = log [ x + sqrt(x*x-1) ]
 *      we have
 *              acosh(x) := log(x)+ln2, if x is large; else
 *              acosh(x) := log(2x-1/(sqrt(x*x-1)+x)) if x>2; else
 *              acosh(x) := log1p(t+sqrt(2.0*t+t*t)); where t=x-1.
 *
 * Special cases:
 *      acosh(x) is NaN with signal if x<1.
 *      acosh(NaN) is NaN without signal.
 */
pub fn acosh(x: f64) -> f64 {
    const one: f64 = 1.0;
    const ln2: f64 = 6.93147180559945286227e-01; /* 0x3FE62E42, 0xFEFA39EF */
    let t: f64;
    let hx = high_word(x);
    let lx = low_word(x);
    if hx < 0x3FF00000 {
        /* x < 1 */
        f64::NAN
    } else if hx >= 0x41B00000 {
        /* x > 2**28 */
        if hx >= 0x7FF00000 {
            /* x is inf of NaN */
            x + x
        } else {
            log(x) + ln2 /* acosh(huge)=log(2x) */
        }
    } else if (((hx - 0x3FF00000) as u32) | lx) == 0 {
        0.0 /* acosh(1) = 0 */
    } else if hx > 0x40000000 {
        /* 2**28 > x > 2 */
        t = x * x;
        log(2.0 * x - one / (x + (t - one).sqrt()))
    } else {
        /* 1<x<2 */
        t = x - one;
        log1p(t + (2.0 * t + t * t).sqrt())
    }
}

/* asin(x)
 * Method :
 *      Since  asin(x) = x + x^3/6 + x^5*3/40 + x^7*15/336 + ...
 *      we approximate asin(x) on [0,0.5] by
 *              asin(x) = x + x*x^2*R(x^2)
 *      where
 *              R(x^2) is a rational approximation of (asin(x)-x)/x^3
 *      and its remez error is bounded by
 *              |(asin(x)-x)/x^3 - R(x^2)| < 2^(-58.75)
 *
 *      For x in [0.5,1]
 *              asin(x) = pi/2-2*asin(sqrt((1-x)/2))
 *      Let y = (1-x), z = y/2, s := sqrt(z), and pio2_hi+pio2_lo=pi/2;
 *      then for x>0.98
 *              asin(x) = pi/2 - 2*(s+s*z*R(z))
 *                      = pio2_hi - (2*(s+s*z*R(z)) - pio2_lo)
 *      For x<=0.98, let pio4_hi = pio2_hi/2, then
 *              f = hi part of s;
 *              c = sqrt(z) - f = (z-f*f)/(s+f)         ...f+c=sqrt(z)
 *      and
 *              asin(x) = pi/2 - 2*(s+s*z*R(z))
 *                      = pio4_hi+(pio4-2s)-(2s*z*R(z)-pio2_lo)
 *                      = pio4_hi+(pio4-2f)-(2s*z*R(z)-(pio2_lo+2c))
 *
 * Special cases:
 *      if x is NaN, return x itself;
 *      if |x|>1, return NaN with invalid signal.
 */
pub fn asin(x: f64) -> f64 {
    const one: f64 = 1.00000000000000000000e+00; /* 0x3FF00000, 0x00000000 */
    const huge: f64 = 1.000e+300;
    const pio2_hi: f64 = 1.57079632679489655800e+00; /* 0x3FF921FB, 0x54442D18 */
    const pio2_lo: f64 = 6.12323399573676603587e-17; /* 0x3C91A626, 0x33145C07 */
    const pio4_hi: f64 = 7.85398163397448278999e-01; /* 0x3FE921FB, 0x54442D18 */
    /* coefficient for R(x^2) */
    const pS0: f64 = 1.66666666666666657415e-01; /* 0x3FC55555, 0x55555555 */
    const pS1: f64 = -3.25565818622400915405e-01; /* 0xBFD4D612, 0x03EB6F7D */
    const pS2: f64 = 2.01212532134862925881e-01; /* 0x3FC9C155, 0x0E884455 */
    const pS3: f64 = -4.00555345006794114027e-02; /* 0xBFA48228, 0xB5688F3B */
    const pS4: f64 = 7.91534994289814532176e-04; /* 0x3F49EFE0, 0x7501B288 */
    const pS5: f64 = 3.47933107596021167570e-05; /* 0x3F023DE1, 0x0DFDF709 */
    const qS1: f64 = -2.40339491173441421878e+00; /* 0xC0033A27, 0x1C8A2D4B */
    const qS2: f64 = 2.02094576023350569471e+00; /* 0x40002AE5, 0x9C598AC8 */
    const qS3: f64 = -6.88283971605453293030e-01; /* 0xBFE6066C, 0x1B8D0159 */
    const qS4: f64 = 7.70381505559019352791e-02; /* 0x3FB3B8C5, 0xB12E9282 */

    let mut t: f64;
    let mut w: f64;
    let mut p: f64;
    let mut q: f64;
    let c: f64;
    let r: f64;
    let s: f64;

    t = 0.0;
    let hx = high_word(x);
    let ix = hx & 0x7FFFFFFF;
    if ix >= 0x3FF00000 {
        /* |x|>= 1 */
        let lx = low_word(x);
        if (((ix - 0x3FF00000) as u32) | lx) == 0 {
            /* asin(1)=+-pi/2 with inexact */
            return x * pio2_hi + x * pio2_lo;
        }
        return f64::NAN; // asin(|x|>1) is NaN
    } else if ix < 0x3FE00000 {
        /* |x|<0.5 */
        if ix < 0x3E400000 {
            /* if |x| < 2**-27 */
            if huge + x > one {
                return x; /* return x with inexact if x!=0*/
            }
        } else {
            t = x * x;
        }
        p = t * (pS0 + t * (pS1 + t * (pS2 + t * (pS3 + t * (pS4 + t * pS5)))));
        q = one + t * (qS1 + t * (qS2 + t * (qS3 + t * qS4)));
        w = p / q;
        return x + x * w;
    }
    /* 1> |x|>= 0.5 */
    w = one - x.abs();
    t = w * 0.5;
    p = t * (pS0 + t * (pS1 + t * (pS2 + t * (pS3 + t * (pS4 + t * pS5)))));
    q = one + t * (qS1 + t * (qS2 + t * (qS3 + t * qS4)));
    s = t.sqrt();
    if ix >= 0x3FEF3333 {
        /* if |x| > 0.975 */
        w = p / q;
        t = pio2_hi - (2.0 * (s + s * w) - pio2_lo);
    } else {
        w = s;
        w = set_low_word(w, 0);
        c = (t - w * w) / (s + w);
        r = p / q;
        p = 2.0 * s * r - (pio2_lo - 2.0 * c);
        q = pio4_hi - 2.0 * w;
        t = pio4_hi - (p - q);
    }
    if hx > 0 {
        t
    } else {
        -t
    }
}

/* asinh(x)
 * Method :
 *      Based on
 *              asinh(x) = sign(x) * log [ |x| + sqrt(x*x+1) ]
 *      we have
 *      asinh(x) := x  if  1+x*x=1,
 *               := sign(x)*(log(x)+ln2)) for large |x|, else
 *               := sign(x)*log(2|x|+1/(|x|+sqrt(x*x+1))) if|x|>2, else
 *               := sign(x)*log1p(|x| + x^2/(1 + sqrt(1+x^2)))
 */
pub fn asinh(x: f64) -> f64 {
    const one: f64 = 1.00000000000000000000e+00; /* 0x3FF00000, 0x00000000 */
    const ln2: f64 = 6.93147180559945286227e-01; /* 0x3FE62E42, 0xFEFA39EF */
    const huge: f64 = 1.00000000000000000000e+300;

    let t: f64;
    let w: f64;
    let hx = high_word(x);
    let ix = hx & 0x7FFFFFFF;
    if ix >= 0x7FF00000 {
        return x + x; /* x is inf or NaN */
    }
    if ix < 0x3E300000 {
        /* |x|<2**-28 */
        if huge + x > one {
            return x; /* return x inexact except 0 */
        }
    }
    if ix > 0x41B00000 {
        /* |x| > 2**28 */
        w = log(x.abs()) + ln2;
    } else if ix > 0x40000000 {
        /* 2**28 > |x| > 2.0 */
        t = x.abs();
        w = log(2.0 * t + one / ((x * x + one).sqrt() + t));
    } else {
        /* 2.0 > |x| > 2**-28 */
        t = x * x;
        w = log1p(x.abs() + t / (one + (one + t).sqrt()));
    }
    if hx > 0 {
        w
    } else {
        -w
    }
}

/* atan(x)
 * Method
 *   1. Reduce x to positive by atan(x) = -atan(-x).
 *   2. According to the integer k=4t+0.25 chopped, t=x, the argument
 *      is further reduced to one of the following intervals and the
 *      arctangent of t is evaluated by the corresponding formula:
 *
 *      [0,7/16]      atan(x) = t-t^3*(a1+t^2*(a2+...(a10+t^2*a11)...)
 *      [7/16,11/16]  atan(x) = atan(1/2) + atan( (t-0.5)/(1+t/2) )
 *      [11/16.19/16] atan(x) = atan( 1 ) + atan( (t-1)/(1+t) )
 *      [19/16,39/16] atan(x) = atan(3/2) + atan( (t-1.5)/(1+1.5t) )
 *      [39/16,INF]   atan(x) = atan(INF) + atan( -1/t )
 */
pub fn atan(mut x: f64) -> f64 {
    static atanhi: [f64; 4] = [
        4.63647609000806093515e-01, /* atan(0.5)hi 0x3FDDAC67, 0x0561BB4F */
        7.85398163397448278999e-01, /* atan(1.0)hi 0x3FE921FB, 0x54442D18 */
        9.82793723247329054082e-01, /* atan(1.5)hi 0x3FEF730B, 0xD281F69B */
        1.57079632679489655800e+00, /* atan(inf)hi 0x3FF921FB, 0x54442D18 */
    ];

    static atanlo: [f64; 4] = [
        2.26987774529616870924e-17, /* atan(0.5)lo 0x3C7A2B7F, 0x222F65E2 */
        3.06161699786838301793e-17, /* atan(1.0)lo 0x3C81A626, 0x33145C07 */
        1.39033110312309984516e-17, /* atan(1.5)lo 0x3C700788, 0x7AF0CBBD */
        6.12323399573676603587e-17, /* atan(inf)lo 0x3C91A626, 0x33145C07 */
    ];

    static aT: [f64; 11] = [
        3.33333333333329318027e-01,  /* 0x3FD55555, 0x5555550D */
        -1.99999999998764832476e-01, /* 0xBFC99999, 0x9998EBC4 */
        1.42857142725034663711e-01,  /* 0x3FC24924, 0x920083FF */
        -1.11111104054623557880e-01, /* 0xBFBC71C6, 0xFE231671 */
        9.09088713343650656196e-02,  /* 0x3FB745CD, 0xC54C206E */
        -7.69187620504482999495e-02, /* 0xBFB3B0F2, 0xAF749A6D */
        6.66107313738753120669e-02,  /* 0x3FB10D66, 0xA0D03D51 */
        -5.83357013379057348645e-02, /* 0xBFADDE2D, 0x52DEFD9A */
        4.97687799461593236017e-02,  /* 0x3FA97B4B, 0x24760DEB */
        -3.65315727442169155270e-02, /* 0xBFA2B444, 0x2C6A6C2F */
        1.62858201153657823623e-02,  /* 0x3F90AD3A, 0xE322DA11 */
    ];

    const one: f64 = 1.0;
    const huge: f64 = 1.0e300;

    let w: f64;
    let s1: f64;
    let s2: f64;
    let mut z: f64;
    let id: i32;

    let hx = high_word(x);
    let ix = hx & 0x7FFFFFFF;
    if ix >= 0x44100000 {
        /* if |x| >= 2^66 */
        let low = low_word(x);
        if ix > 0x7FF00000 || (ix == 0x7FF00000 && (low != 0)) {
            return x + x; /* NaN */
        }
        if hx > 0 {
            return atanhi[3] + atanlo[3];
        } else {
            return -atanhi[3] - atanlo[3];
        }
    }
    if ix < 0x3FDC0000 {
        /* |x| < 0.4375 */
        if ix < 0x3E400000 {
            /* |x| < 2^-27 */
            if huge + x > one {
                return x; /* raise inexact */
            }
        }
        id = -1;
    } else {
        x = x.abs();
        if ix < 0x3FF30000 {
            /* |x| < 1.1875 */
            if ix < 0x3FE60000 {
                /* 7/16 <=|x|<11/16 */
                id = 0;
                x = (2.0 * x - one) / (2.0 + x);
            } else {
                /* 11/16<=|x|< 19/16 */
                id = 1;
                x = (x - one) / (x + one);
            }
        } else if ix < 0x40038000 {
            /* |x| < 2.4375 */
            id = 2;
            x = (x - 1.5) / (one + 1.5 * x);
        } else {
            /* 2.4375 <= |x| < 2^66 */
            id = 3;
            x = -1.0 / x;
        }
    }
    /* end of argument reduction */
    z = x * x;
    w = z * z;
    /* break sum from i=0 to 10 aT[i]z**(i+1) into odd and even poly */
    s1 = z * (aT[0] + w * (aT[2] + w * (aT[4] + w * (aT[6] + w * (aT[8] + w * aT[10])))));
    s2 = w * (aT[1] + w * (aT[3] + w * (aT[5] + w * (aT[7] + w * aT[9]))));
    if id < 0 {
        x - x * (s1 + s2)
    } else {
        z = atanhi[id as usize] - ((x * (s1 + s2) - atanlo[id as usize]) - x);
        if hx < 0 {
            -z
        } else {
            z
        }
    }
}

/* atan2(y,x)
 * Method :
 *  1. Reduce y to positive by atan2(y,x)=-atan2(-y,x).
 *  2. Reduce x to positive by (if x and y are unexceptional):
 *    ARG (x+iy) = arctan(y/x)       ... if x > 0,
 *    ARG (x+iy) = pi - arctan[y/(-x)]   ... if x < 0,
 *
 * Special cases:
 *
 *  ATAN2((anything), NaN ) is NaN;
 *  ATAN2(NAN , (anything) ) is NaN;
 *  ATAN2(+-0, +(anything but NaN)) is +-0  ;
 *  ATAN2(+-0, -(anything but NaN)) is +-pi ;
 *  ATAN2(+-(anything but 0 and NaN), 0) is +-pi/2;
 *  ATAN2(+-(anything but INF and NaN), +INF) is +-0 ;
 *  ATAN2(+-(anything but INF and NaN), -INF) is +-pi;
 *  ATAN2(+-INF,+INF ) is +-pi/4 ;
 *  ATAN2(+-INF,-INF ) is +-3pi/4;
 *  ATAN2(+-INF, (anything but,0,NaN, and INF)) is +-pi/2;
 */
pub fn atan2(y: f64, x: f64) -> f64 {
    const tiny: f64 = 1.0e-300;
    const zero: f64 = 0.0;
    const pi_o_4: f64 = 7.8539816339744827900E-01; /* 0x3FE921FB, 0x54442D18 */
    const pi_o_2: f64 = 1.5707963267948965580E+00; /* 0x3FF921FB, 0x54442D18 */
    const pi: f64 = 3.1415926535897931160E+00; /* 0x400921FB, 0x54442D18 */
    const pi_lo: f64 = 1.2246467991473531772E-16; /* 0x3CA1A626, 0x33145C07 */

    let z: f64;
    let k: i32;
    let mut m: i32;

    let hx = high_word(x);
    let lx = low_word(x);
    let ix = hx & 0x7FFFFFFF;
    let hy = high_word(y);
    let ly = low_word(y);
    let iy = hy & 0x7FFFFFFF;
    if ((ix as u32 | ((lx | lx.wrapping_neg()) >> 31)) > 0x7FF00000)
        || ((iy as u32 | ((ly | ly.wrapping_neg()) >> 31)) > 0x7FF00000)
    {
        return x + y; /* x or y is NaN */
    }
    if (hx.wrapping_sub(0x3FF00000) as u32 | lx) == 0 {
        return atan(y); /* x=1.0 */
    }
    m = ((hy >> 31) & 1) | ((hx >> 30) & 2); /* 2*sign(x)+sign(y) */

    /* when y = 0 */
    if (iy as u32 | ly) == 0 {
        match m {
            0 | 1 => return y,         /* atan(+-0,+anything)=+-0 */
            2 => return pi + tiny,     /* atan(+0,-anything) = pi */
            _ => return -pi - tiny,    /* atan(-0,-anything) =-pi */
        }
    }
    /* when x = 0 */
    if (ix as u32 | lx) == 0 {
        return if hy < 0 { -pi_o_2 - tiny } else { pi_o_2 + tiny };
    }

    /* when x is INF */
    if ix == 0x7FF00000 {
        if iy == 0x7FF00000 {
            match m {
                0 => return pi_o_4 + tiny,        /* atan(+INF,+INF) */
                1 => return -pi_o_4 - tiny,       /* atan(-INF,+INF) */
                2 => return 3.0 * pi_o_4 + tiny,  /* atan(+INF,-INF) */
                _ => return -3.0 * pi_o_4 - tiny, /* atan(-INF,-INF) */
            }
        } else {
            match m {
                0 => return zero,       /* atan(+...,+INF) */
                1 => return -zero,      /* atan(-...,+INF) */
                2 => return pi + tiny,  /* atan(+...,-INF) */
                _ => return -pi - tiny, /* atan(-...,-INF) */
            }
        }
    }
    /* when y is INF */
    if iy == 0x7FF00000 {
        return if hy < 0 { -pi_o_2 - tiny } else { pi_o_2 + tiny };
    }

    /* compute y/x */
    k = (iy - ix) >> 20;
    if k > 60 {
        /* |y/x| >  2**60 */
        z = pi_o_2 + 0.5 * pi_lo;
        m &= 1;
    } else if hx < 0 && k < -60 {
        z = 0.0; /* 0 > |y|/x > -2**-60 */
    } else {
        z = atan((y / x).abs()); /* safe to do y/x */
    }
    match m {
        0 => z,                  /* atan(+,+) */
        1 => -z,                 /* atan(-,+) */
        2 => pi - (z - pi_lo),   /* atan(+,-) */
        _ => (z - pi_lo) - pi,   /* atan(-,-) */
    }
}

/* cos(x)
 * Return cosine function of x.
 *
 * kernel function:
 *      __kernel_sin            ... sine function on [-pi/4,pi/4]
 *      __kernel_cos            ... cosine function on [-pi/4,pi/4]
 *      __ieee754_rem_pio2      ... argument reduction routine
 *
 * Method.
 *      Let S,C and T denote the sin, cos and tan respectively on
 *      [-PI/4, +PI/4]. Reduce the argument x to y1+y2 = x-k*pi/2
 *      in [-pi/4 , +pi/4], and let n = k mod 4.
 *      We have
 *
 *          n        sin(x)      cos(x)        tan(x)
 *     ----------------------------------------------------------
 *          0          S           C             T
 *          1          C          -S            -1/T
 *          2         -S          -C             T
 *          3         -C           S            -1/T
 *     ----------------------------------------------------------
 *
 * Special cases:
 *      Let trig be any of sin, cos, or tan.
 *      trig(+-INF)  is NaN, with signals;
 *      trig(NaN)    is that NaN;
 *
 * Accuracy:
 *      TRIG(x) returns trig(x) nearly rounded
 *
 * This is the fdlibm_cos body (the V8_USE_LIBM_TRIG_FUNCTIONS=off path).
 */
pub fn cos(x: f64) -> f64 {
    let mut y = [0.0f64; 2];
    let z = 0.0;

    /* High word of x. */
    let mut ix = high_word(x);

    /* |x| ~< pi/4 */
    ix &= 0x7FFFFFFF;
    if ix <= 0x3FE921FB {
        kernel_cos(x, z)
    } else if ix >= 0x7FF00000 {
        /* cos(Inf or NaN) is NaN */
        x - x
    } else {
        /* argument reduction needed */
        let n = rem_pio2(x, &mut y);
        match n & 3 {
            0 => kernel_cos(y[0], y[1]),
            1 => -kernel_sin(y[0], y[1], 1),
            2 => -kernel_cos(y[0], y[1]),
            _ => kernel_sin(y[0], y[1], 1),
        }
    }
}

/* scalbn is not in ieee754.cc: V8 calls libm's scalbn. This is the fdlibm s_scalbn.c
 * algorithm (glibc uses the same one). It is exact except for rounding in the
 * subnormal case, which is one multiplication by 2**-54. */
fn scalbn(mut x: f64, n: i32) -> f64 {
    const two54: f64 = 1.80143985094819840000e+16; /* 0x43500000, 0x00000000 */
    const twom54: f64 = 5.55111512312578270212e-17; /* 0x3C900000, 0x00000000 */
    const huge: f64 = 1.0e+300;
    const tiny: f64 = 1.0e-300;

    let mut hx = high_word(x);
    let lx = low_word(x);
    let mut k = (hx & 0x7ff00000) >> 20; /* extract exponent */
    if k == 0 {
        /* 0 or subnormal x */
        if (lx | (hx & 0x7fffffff) as u32) == 0 {
            return x; /* +-0 */
        }
        x *= two54;
        hx = high_word(x);
        k = ((hx & 0x7ff00000) >> 20) - 54;
        if n < -50000 {
            return tiny * x; /*underflow*/
        }
    }
    if k == 0x7ff {
        return x + x; /* NaN or Inf */
    }
    k += n;
    if k > 0x7fe {
        return huge * huge.copysign(x); /* overflow  */
    }
    if k > 0 {
        /* normal result */
        return set_high_word(x, (hx & 0x800fffffu32 as i32) | (k << 20));
    }
    if k <= -54 {
        if n > 50000 {
            /* in case integer overflow in n+k */
            return huge * huge.copysign(x); /*overflow*/
        } else {
            return tiny * tiny.copysign(x); /*underflow*/
        }
    }
    k += 54; /* subnormal result */
    x = set_high_word(x, (hx & 0x800fffffu32 as i32) | (k << 20));
    x * twom54
}

/* exp(x)
 * Returns the exponential of x.
 *
 * Method
 *   1. Argument reduction:
 *      Reduce x to an r so that |r| <= 0.5*ln2 ~ 0.34658.
 *      Given x, find r and integer k such that
 *
 *               x = k*ln2 + r,  |r| <= 0.5*ln2.
 *
 *      Here r will be represented as r = hi-lo for better
 *      accuracy.
 *
 *   2. Approximation of exp(r) by a special rational function on
 *      the interval [0,0.34658] (see V8's ieee754.cc for the full notes).
 *
 *   3. Scale back to obtain exp(x):
 *      From step 1, we have
 *         exp(x) = 2^k * exp(r)
 *
 * Special cases:
 *      exp(INF) is INF, exp(NaN) is NaN;
 *      exp(-INF) is 0, and
 *      for finite argument, only exp(0)=1 is exact.
 *
 * Misc. info.
 *      For IEEE double
 *          if x >  7.09782712893383973096e+02 then exp(x) overflow
 *          if x < -7.45133219101941108420e+02 then exp(x) underflow
 */
pub fn exp(mut x: f64) -> f64 {
    const one: f64 = 1.0;
    static halF: [f64; 2] = [0.5, -0.5];
    const o_threshold: f64 = 7.09782712893383973096e+02; /* 0x40862E42, 0xFEFA39EF */
    const u_threshold: f64 = -7.45133219101941108420e+02; /* 0xC0874910, 0xD52D3051 */
    static ln2HI: [f64; 2] = [
        6.93147180369123816490e-01,  /* 0x3FE62E42, 0xFEE00000 */
        -6.93147180369123816490e-01, /* 0xBFE62E42, 0xFEE00000 */
    ];
    static ln2LO: [f64; 2] = [
        1.90821492927058770002e-10,  /* 0x3DEA39EF, 0x35793C76 */
        -1.90821492927058770002e-10, /* 0xBDEA39EF, 0x35793C76 */
    ];
    const invln2: f64 = 1.44269504088896338700e+00; /* 0x3FF71547, 0x652B82FE */
    const P1: f64 = 1.66666666666666019037e-01; /* 0x3FC55555, 0x5555553E */
    const P2: f64 = -2.77777777770155933842e-03; /* 0xBF66C16C, 0x16BEBD93 */
    const P3: f64 = 6.61375632143793436117e-05; /* 0x3F11566A, 0xAF25DE2C */
    const P4: f64 = -1.65339022054652515390e-06; /* 0xBEBBBD41, 0xC5D26BF1 */
    const P5: f64 = 4.13813679705723846039e-08; /* 0x3E663769, 0x72BEA4D0 */
    const E: f64 = 2.718281828459045; /* 0x4005BF0A, 0x8B145769 */

    const huge: f64 = 1.0e+300;
    const twom1000: f64 = 9.33263618503218878990e-302; /* 2**-1000=0x01700000,0*/
    const two1023: f64 = 8.988465674311579539e307; /* 0x1p1023 */

    let y: f64;
    let mut hi = 0.0f64;
    let mut lo = 0.0f64;
    let c: f64;
    let mut t: f64;
    let twopk: f64;
    let mut k: i32 = 0;

    let mut hx = high_word(x) as u32;
    let xsb = ((hx >> 31) & 1) as usize; /* sign bit of x */
    hx &= 0x7FFFFFFF; /* high word of |x| */

    /* filter out non-finite argument */
    if hx >= 0x40862E42 {
        /* if |x|>=709.78... */
        if hx >= 0x7FF00000 {
            let lx = low_word(x);
            if ((hx & 0xFFFFF) | lx) != 0 {
                return x + x; /* NaN */
            } else {
                return if xsb == 0 { x } else { 0.0 }; /* exp(+-inf)={inf,0} */
            }
        }
        if x > o_threshold {
            return huge * huge; /* overflow */
        }
        if x < u_threshold {
            return twom1000 * twom1000; /* underflow */
        }
    }

    /* argument reduction */
    if hx > 0x3FD62E42 {
        /* if  |x| > 0.5 ln2 */
        if hx < 0x3FF0A2B2 {
            /* and |x| < 1.5 ln2 */
            /* TODO(rtoy): We special case exp(1) here to return the correct
             * value of E, as the computation below would get the last bit
             * wrong. We should probably fix the algorithm instead.
             */
            if x == 1.0 {
                return E;
            }
            hi = x - ln2HI[xsb];
            lo = ln2LO[xsb];
            k = 1 - xsb as i32 - xsb as i32;
        } else {
            k = (invln2 * x + halF[xsb]) as i32;
            t = k as f64;
            hi = x - t * ln2HI[0]; /* t*ln2HI is exact here */
            lo = t * ln2LO[0];
        }
        x = hi - lo;
    } else if hx < 0x3E300000 {
        /* when |x|<2**-28 */
        if huge + x > one {
            return one + x; /* trigger inexact */
        }
    } else {
        k = 0;
    }

    /* x is now in primary range */
    t = x * x;
    if k >= -1021 {
        twopk = insert_words(0x3FF00000i32.wrapping_add(((k as u32) << 20) as i32), 0);
    } else {
        twopk = insert_words(0x3FF00000u32.wrapping_add(((k + 1000) as u32) << 20) as i32, 0);
    }
    c = x - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    if k == 0 {
        return one - ((x * c) / (c - 2.0) - x);
    } else {
        y = one - ((lo - (x * c) / (2.0 - c)) - hi);
    }
    if k >= -1021 {
        if k == 1024 {
            return y * 2.0 * two1023;
        }
        y * twopk
    } else {
        y * twopk * twom1000
    }
}

/*
 * Method :
 *    1.Reduced x to positive by atanh(-x) = -atanh(x)
 *    2.For x>=0.5
 *              1              2x                          x
 *  atanh(x) = --- * log(1 + -------) = 0.5 * log1p(2 * --------)
 *              2             1 - x                      1 - x
 *
 *   For x<0.5
 *  atanh(x) = 0.5*log1p(2x+2x*x/(1-x))
 *
 * Special cases:
 *  atanh(x) is NaN if |x| > 1 with signal;
 *  atanh(NaN) is that NaN with no signal;
 *  atanh(+-1) is +-INF with signal.
 *
 */
pub fn atanh(mut x: f64) -> f64 {
    const one: f64 = 1.0;
    const huge: f64 = 1e300;
    const zero: f64 = 0.0;

    let mut t: f64;
    let hx = high_word(x);
    let lx = low_word(x);
    let ix = hx & 0x7FFFFFFF;
    if ((ix as u32) | ((lx | lx.wrapping_neg()) >> 31)) > 0x3FF00000 {
        /* |x|>1 */
        return f64::NAN;
    }
    if ix == 0x3FF00000 {
        return if x > 0.0 { f64::INFINITY } else { f64::NEG_INFINITY };
    }
    if ix < 0x3E300000 && (huge + x) > zero {
        return x; /* x<2**-28 */
    }
    x = set_high_word(x, ix);
    if ix < 0x3FE00000 {
        /* x < 0.5 */
        t = x + x;
        t = 0.5 * log1p(t + t * x / (one - x));
    } else {
        t = 0.5 * log1p((x + x) / (one - x));
    }
    if hx >= 0 {
        t
    } else {
        -t
    }
}

/* log(x)
 * Return the logrithm of x
 *
 * Method :
 *   1. Argument Reduction: find k and f such that
 *     x = 2^k * (1+f),
 *     where  sqrt(2)/2 < 1+f < sqrt(2) .
 *
 *   2. Approximation of log(1+f).
 *  Let s = f/(2+f) ; based on log(1+f) = log(1+s) - log(1-s)
 *     = 2s + 2/3 s**3 + 2/5 s**5 + .....,
 *         = 2s + s*R
 *  (see V8's ieee754.cc for the full notes)
 *
 *   3. Finally,  log(x) = k*ln2 + log(1+f).
 *          = k*ln2_hi+(f-(hfsq-(s*(hfsq+R)+k*ln2_lo)))
 *     Here ln2 is split into two floating point number:
 *      ln2_hi + ln2_lo,
 *     where n*ln2_hi is always exact for |n| < 2000.
 *
 * Special cases:
 *  log(x) is NaN with signal if x < 0 (including -INF) ;
 *  log(+INF) is +INF; log(0) is -INF with signal;
 *  log(NaN) is that NaN with no signal.
 */
pub fn log(mut x: f64) -> f64 {
    const ln2_hi: f64 = 6.93147180369123816490e-01; /* 3fe62e42 fee00000 */
    const ln2_lo: f64 = 1.90821492927058770002e-10; /* 3dea39ef 35793c76 */
    const two54: f64 = 1.80143985094819840000e+16; /* 43500000 00000000 */
    const Lg1: f64 = 6.666666666666735130e-01; /* 3FE55555 55555593 */
    const Lg2: f64 = 3.999999999940941908e-01; /* 3FD99999 9997FA04 */
    const Lg3: f64 = 2.857142874366239149e-01; /* 3FD24924 94229359 */
    const Lg4: f64 = 2.222219843214978396e-01; /* 3FCC71C5 1D8E78AF */
    const Lg5: f64 = 1.818357216161805012e-01; /* 3FC74664 96CB03DE */
    const Lg6: f64 = 1.531383769920937332e-01; /* 3FC39A09 D078C69F */
    const Lg7: f64 = 1.479819860511658591e-01; /* 3FC2F112 DF3E5244 */

    const zero: f64 = 0.0;

    let hfsq: f64;
    let f: f64;
    let s: f64;
    let z: f64;
    let R: f64;
    let w: f64;
    let t1: f64;
    let t2: f64;
    let dk: f64;
    let mut k: i32;
    let mut hx: i32;
    let mut i: i32;
    let j: i32;
    let lx: u32;

    hx = high_word(x);
    lx = low_word(x);

    k = 0;
    if hx < 0x00100000 {
        /* x < 2**-1022  */
        if (((hx & 0x7FFFFFFF) as u32) | lx) == 0 {
            return f64::NEG_INFINITY; /* log(+-0)=-inf */
        }
        if hx < 0 {
            return f64::NAN; /* log(-#) = NaN */
        }
        k -= 54;
        x *= two54; /* subnormal number, scale up x */
        hx = high_word(x);
    }
    if hx >= 0x7FF00000 {
        return x + x;
    }
    k += (hx >> 20) - 1023;
    hx &= 0x000FFFFF;
    i = (hx + 0x95F64) & 0x100000;
    x = set_high_word(x, hx | (i ^ 0x3FF00000)); /* normalize x or x/2 */
    k += i >> 20;
    f = x - 1.0;
    if (0x000FFFFF & (2 + hx)) < 3 {
        /* -2**-20 <= f < 2**-20 */
        if f == zero {
            if k == 0 {
                return zero;
            } else {
                dk = k as f64;
                return dk * ln2_hi + dk * ln2_lo;
            }
        }
        R = f * f * (0.5 - 0.33333333333333333 * f);
        if k == 0 {
            return f - R;
        } else {
            dk = k as f64;
            return dk * ln2_hi - ((R - dk * ln2_lo) - f);
        }
    }
    s = f / (2.0 + f);
    dk = k as f64;
    z = s * s;
    i = hx - 0x6147A;
    w = z * z;
    j = 0x6B851 - hx;
    t1 = w * (Lg2 + w * (Lg4 + w * Lg6));
    t2 = z * (Lg1 + w * (Lg3 + w * (Lg5 + w * Lg7)));
    i |= j;
    R = t2 + t1;
    if i > 0 {
        hfsq = 0.5 * f * f;
        if k == 0 {
            f - (hfsq - s * (hfsq + R))
        } else {
            dk * ln2_hi - ((hfsq - (s * (hfsq + R) + dk * ln2_lo)) - f)
        }
    } else if k == 0 {
        f - s * (f - R)
    } else {
        dk * ln2_hi - ((s * (f - R) - dk * ln2_lo) - f)
    }
}

/* double log1p(double x)
 *
 * Method :
 *   1. Argument Reduction: find k and f such that
 *      1+x = 2^k * (1+f),
 *     where  sqrt(2)/2 < 1+f < sqrt(2) .
 *
 *      Note. If k=0, then f=x is exact. However, if k!=0, then f
 *  may not be representable exactly. In that case, a correction
 *  term is need. Let u=1+x rounded. Let c = (1+x)-u, then
 *  log(1+x) - log(u) ~ c/u. Thus, we proceed to compute log(u),
 *  and add back the correction term c/u.
 *  (Note: when x > 2**53, one can simply return log(x))
 *
 *   2. Approximation of log1p(f). (see V8's ieee754.cc for the full notes)
 *
 *   3. Finally, log1p(x) = k*ln2 + log1p(f).
 *           = k*ln2_hi+(f-(hfsq-(s*(hfsq+R)+k*ln2_lo)))
 *
 * Special cases:
 *  log1p(x) is NaN with signal if x < -1 (including -INF) ;
 *  log1p(+INF) is +INF; log1p(-1) is -INF with signal;
 *  log1p(NaN) is that NaN with no signal.
 */
pub fn log1p(x: f64) -> f64 {
    const ln2_hi: f64 = 6.93147180369123816490e-01; /* 3fe62e42 fee00000 */
    const ln2_lo: f64 = 1.90821492927058770002e-10; /* 3dea39ef 35793c76 */
    const two54: f64 = 1.80143985094819840000e+16; /* 43500000 00000000 */
    const Lp1: f64 = 6.666666666666735130e-01; /* 3FE55555 55555593 */
    const Lp2: f64 = 3.999999999940941908e-01; /* 3FD99999 9997FA04 */
    const Lp3: f64 = 2.857142874366239149e-01; /* 3FD24924 94229359 */
    const Lp4: f64 = 2.222219843214978396e-01; /* 3FCC71C5 1D8E78AF */
    const Lp5: f64 = 1.818357216161805012e-01; /* 3FC74664 96CB03DE */
    const Lp6: f64 = 1.531383769920937332e-01; /* 3FC39A09 D078C69F */
    const Lp7: f64 = 1.479819860511658591e-01; /* 3FC2F112 DF3E5244 */

    const zero: f64 = 0.0;

    let hfsq: f64;
    let mut f: f64 = 0.0;
    let mut c: f64 = 0.0;
    let s: f64;
    let z: f64;
    let R: f64;
    let mut u: f64;
    let mut k: i32;
    let mut hu: i32 = 0;

    let hx = high_word(x);
    let ax = hx & 0x7FFFFFFF;

    k = 1;
    if hx < 0x3FDA827A {
        /* 1+x < sqrt(2)+ */
        if ax >= 0x3FF00000 {
            /* x <= -1.0 */
            if x == -1.0 {
                return f64::NEG_INFINITY; /* log1p(-1)=+inf */
            } else {
                return f64::NAN; // log1p(x<-1)=NaN
            }
        }
        if ax < 0x3E200000 {
            /* |x| < 2**-29 */
            if two54 + x > zero /* raise inexact */
                && ax < 0x3C900000
            /* |x| < 2**-54 */
            {
                return x;
            } else {
                return x - x * x * 0.5;
            }
        }
        if hx > 0 || hx <= 0xBFD2BEC4u32 as i32 {
            k = 0;
            f = x;
            hu = 1;
        } /* sqrt(2)/2- <= 1+x < sqrt(2)+ */
    }
    if hx >= 0x7FF00000 {
        return x + x;
    }
    if k != 0 {
        if hx < 0x43400000 {
            u = 1.0 + x;
            hu = high_word(u);
            k = (hu >> 20) - 1023;
            c = if k > 0 { 1.0 - (u - x) } else { x - (u - 1.0) }; /* correction term */
            c /= u;
        } else {
            u = x;
            hu = high_word(u);
            k = (hu >> 20) - 1023;
            c = 0.0;
        }
        hu &= 0x000FFFFF;
        /*
         * The approximation to sqrt(2) used in thresholds is not
         * critical.  However, the ones used above must give less
         * strict bounds than the one here so that the k==0 case is
         * never reached from here, since here we have committed to
         * using the correction term but don't use it if k==0.
         */
        if hu < 0x6A09E {
            /* u ~< sqrt(2) */
            u = set_high_word(u, hu | 0x3FF00000); /* normalize u */
        } else {
            k += 1;
            u = set_high_word(u, hu | 0x3FE00000); /* normalize u/2 */
            hu = (0x00100000 - hu) >> 2;
        }
        f = u - 1.0;
    }
    hfsq = 0.5 * f * f;
    if hu == 0 {
        /* |f| < 2**-20 */
        if f == zero {
            if k == 0 {
                return zero;
            } else {
                c += k as f64 * ln2_lo;
                return k as f64 * ln2_hi + c;
            }
        }
        R = hfsq * (1.0 - 0.66666666666666666 * f);
        if k == 0 {
            return f - R;
        } else {
            return k as f64 * ln2_hi - ((R - (k as f64 * ln2_lo + c)) - f);
        }
    }
    s = f / (2.0 + f);
    z = s * s;
    R = z * (Lp1 + z * (Lp2 + z * (Lp3 + z * (Lp4 + z * (Lp5 + z * (Lp6 + z * Lp7))))));
    if k == 0 {
        f - (hfsq - s * (hfsq + R))
    } else {
        k as f64 * ln2_hi - ((hfsq - (s * (hfsq + R) + (k as f64 * ln2_lo + c))) - f)
    }
}

/*
 * k_log1p(f):
 * Return log(1+f) - f for 1+f in ~[sqrt(2)/2, sqrt(2)].
 *
 * The following describes the overall strategy for computing
 * logarithms in base e.  The argument reduction and adding the final
 * term of the polynomial are done by the caller for increased accuracy
 * when different bases are used. (See V8's ieee754.cc for the full notes.)
 */

const Lg1: f64 = 6.666666666666735130e-01; /* 3FE55555 55555593 */
const Lg2: f64 = 3.999999999940941908e-01; /* 3FD99999 9997FA04 */
const Lg3: f64 = 2.857142874366239149e-01; /* 3FD24924 94229359 */
const Lg4: f64 = 2.222219843214978396e-01; /* 3FCC71C5 1D8E78AF */
const Lg5: f64 = 1.818357216161805012e-01; /* 3FC74664 96CB03DE */
const Lg6: f64 = 1.531383769920937332e-01; /* 3FC39A09 D078C69F */
const Lg7: f64 = 1.479819860511658591e-01; /* 3FC2F112 DF3E5244 */

#[inline(always)]
fn k_log1p(f: f64) -> f64 {
    let hfsq: f64;
    let s: f64;
    let z: f64;
    let R: f64;
    let w: f64;
    let t1: f64;
    let t2: f64;

    s = f / (2.0 + f);
    z = s * s;
    w = z * z;
    t1 = w * (Lg2 + w * (Lg4 + w * Lg6));
    t2 = z * (Lg1 + w * (Lg3 + w * (Lg5 + w * Lg7)));
    R = t2 + t1;
    hfsq = 0.5 * f * f;
    s * (hfsq + R)
}

/*
 * Return the base 2 logarithm of x.  See e_log.c and k_log.h for most
 * comments.
 *
 * This reduces x to {k, 1+f} exactly as in e_log.c, then calls the kernel,
 * then does the combining and scaling steps
 *    log2(x) = (f - 0.5*f*f + k_log1p(f)) / ln2 + k
 * in not-quite-routine extra precision.
 */
pub fn log2(mut x: f64) -> f64 {
    const two54: f64 = 1.80143985094819840000e+16; /* 0x43500000, 0x00000000 */
    const ivln2hi: f64 = 1.44269504072144627571e+00; /* 0x3FF71547, 0x65200000 */
    const ivln2lo: f64 = 1.67517131648865118353e-10; /* 0x3DE705FC, 0x2EEFA200 */

    let f: f64;
    let hfsq: f64;
    let mut hi: f64;
    let lo: f64;
    let r: f64;
    let mut val_hi: f64;
    let mut val_lo: f64;
    let w: f64;
    let y: f64;
    let i: i32;
    let mut k: i32;
    let mut hx: i32;

    hx = high_word(x);
    let lx = low_word(x);

    k = 0;
    if hx < 0x00100000 {
        /* x < 2**-1022  */
        if (((hx & 0x7FFFFFFF) as u32) | lx) == 0 {
            return f64::NEG_INFINITY; /* log(+-0)=-inf */
        }
        if hx < 0 {
            return f64::NAN; /* log(-#) = NaN */
        }
        k -= 54;
        x *= two54; /* subnormal number, scale up x */
        hx = high_word(x);
    }
    if hx >= 0x7FF00000 {
        return x + x;
    }
    if hx == 0x3FF00000 && lx == 0 {
        return 0.0; /* log(1) = +0 */
    }
    k += (hx >> 20) - 1023;
    hx &= 0x000FFFFF;
    i = (hx + 0x95F64) & 0x100000;
    x = set_high_word(x, hx | (i ^ 0x3FF00000)); /* normalize x or x/2 */
    k += i >> 20;
    y = k as f64;
    f = x - 1.0;
    hfsq = 0.5 * f * f;
    r = k_log1p(f);

    /*
     * f-hfsq must (for args near 1) be evaluated in extra precision
     * to avoid a large cancellation when x is near sqrt(2) or 1/sqrt(2).
     * y must (for args near sqrt(2) and 1/sqrt(2)) be added in extra
     * precision to avoid a very large cancellation when x is very near
     * these values. This uses Dekker's theorem to normalize y+val_hi.
     */
    hi = f - hfsq;
    hi = set_low_word(hi, 0);
    lo = (f - hi) - hfsq + r;
    val_hi = hi * ivln2hi;
    val_lo = (lo + hi) * ivln2lo + lo * ivln2hi;

    /* spadd(val_hi, val_lo, y), except for not using double_t: */
    w = y + val_hi;
    val_lo += (y - w) + val_hi;
    val_hi = w;

    val_lo + val_hi
}

/*
 * Return the base 10 logarithm of x
 *
 * Method :
 *      Let log10_2hi = leading 40 bits of log10(2) and
 *          log10_2lo = log10(2) - log10_2hi,
 *          ivln10   = 1/log(10) rounded.
 *      Then
 *              n = ilogb(x),
 *              if(n<0)  n = n+1;
 *              x = scalbn(x,-n);
 *              log10(x) := n*log10_2hi + (n*log10_2lo + ivln10*log(x))
 *
 *  Special cases:
 *      log10(x) is NaN if x < 0;
 *      log10(+INF) is +INF; log10(0) is -INF;
 *      log10(NaN) is that NaN;
 *      log10(10**N) = N  for N=0,1,...,22.
 */
pub fn log10(mut x: f64) -> f64 {
    const two54: f64 = 1.80143985094819840000e+16; /* 0x43500000, 0x00000000 */
    const ivln10: f64 = 4.34294481903251816668e-01;
    const log10_2hi: f64 = 3.01029995663611771306e-01; /* 0x3FD34413, 0x509F6000 */
    const log10_2lo: f64 = 3.69423907715893078616e-13; /* 0x3D59FEF3, 0x11F12B36 */

    let y: f64;
    let i: i32;
    let mut k: i32;
    let mut hx: i32;
    let mut lx: u32;

    hx = high_word(x);
    lx = low_word(x);

    k = 0;
    if hx < 0x00100000 {
        /* x < 2**-1022  */
        if (((hx & 0x7FFFFFFF) as u32) | lx) == 0 {
            return f64::NEG_INFINITY; /* log(+-0)=-inf */
        }
        if hx < 0 {
            return f64::NAN; /* log(-#) = NaN */
        }
        k -= 54;
        x *= two54; /* subnormal number, scale up x */
        hx = high_word(x);
        lx = low_word(x);
    }
    if hx >= 0x7FF00000 {
        return x + x;
    }
    if hx == 0x3FF00000 && lx == 0 {
        return 0.0; /* log(1) = +0 */
    }
    k += (hx >> 20) - 1023;

    i = ((k as u32 & 0x80000000) >> 31) as i32;
    hx = (hx & 0x000FFFFF) | ((0x3FF - i) << 20);
    y = (k + i) as f64;
    x = set_high_word(x, hx);
    x = set_low_word(x, lx);

    let z = y * log10_2lo + ivln10 * log(x);
    z + y * log10_2hi
}

/* expm1(x)
 * Returns exp(x)-1, the exponential of x minus 1.
 *
 * Method
 *   1. Argument reduction:
 *  Given x, find r and integer k such that
 *
 *               x = k*ln2 + r,  |r| <= 0.5*ln2 ~ 0.34658
 *
 *      Here a correction term c will be computed to compensate
 *  the error in r when rounded to a floating-point number.
 *
 *   2. Approximating expm1(r) by a special rational function on
 *  the interval [0,0.34658]. (See V8's ieee754.cc for the full notes.)
 *
 *   3. Scale back to obtain expm1(x):
 *  From step 1, we have
 *     expm1(x) = either 2^k*[expm1(r)+1] - 1
 *        = or     2^k*[expm1(r) + (1-2^-k)]
 *   4. Implementation notes:
 *  (A). To save one multiplication, we scale the coefficient Qi
 *       to Qi*2^i, and replace z by (x^2)/2.
 *  (B). To achieve maximum accuracy, we compute expm1(x) by
 *    (i)   if x < -56*ln2, return -1.0, (raise inexact if x!=inf)
 *    (ii)  if k=0, return r-E
 *    (iii) if k=-1, return 0.5*(r-E)-0.5
 *        (iv)  if k=1 if r < -0.25, return 2*((r+0.5)- E)
 *                  else       return  1.0+2.0*(r-E);
 *    (v)   if (k<-2||k>56) return 2^k(1-(E-r)) - 1 (or exp(x)-1)
 *    (vi)  if k <= 20, return 2^k((1-2^-k)-(E-r)), else
 *    (vii) return 2^k(1-((E+2^-k)-r))
 *
 * Special cases:
 *  expm1(INF) is INF, expm1(NaN) is NaN;
 *  expm1(-INF) is -1, and
 *  for finite argument, only expm1(0)=0 is exact.
 */
pub fn expm1(mut x: f64) -> f64 {
    const one: f64 = 1.0;
    const tiny: f64 = 1.0e-300;
    const o_threshold: f64 = 7.09782712893383973096e+02; /* 0x40862E42, 0xFEFA39EF */
    const ln2_hi: f64 = 6.93147180369123816490e-01; /* 0x3FE62E42, 0xFEE00000 */
    const ln2_lo: f64 = 1.90821492927058770002e-10; /* 0x3DEA39EF, 0x35793C76 */
    const invln2: f64 = 1.44269504088896338700e+00; /* 0x3FF71547, 0x652B82FE */
    /* Scaled Q's: Qn_here = 2**n * Qn_above, for R(2*z) where z = hxs =
    x*x/2: */
    const Q1: f64 = -3.33333333333331316428e-02; /* BFA11111 111110F4 */
    const Q2: f64 = 1.58730158725481460165e-03; /* 3F5A01A0 19FE5585 */
    const Q3: f64 = -7.93650757867487942473e-05; /* BF14CE19 9EAADBB7 */
    const Q4: f64 = 4.00821782732936239552e-06; /* 3ED0CFCA 86E65239 */
    const Q5: f64 = -2.01099218183624371326e-07; /* BE8AFDB7 6E09C32D */

    const huge: f64 = 1.0e+300;

    let mut y: f64;
    let hi: f64;
    let lo: f64;
    let mut c: f64 = 0.0;
    let mut t: f64;
    let mut e: f64;
    let hxs: f64;
    let hfx: f64;
    let r1: f64;
    let twopk: f64;
    let k: i32;

    let mut hx = high_word(x) as u32;
    let xsb = hx & 0x80000000; /* sign bit of x */
    hx &= 0x7FFFFFFF; /* high word of |x| */

    /* filter out huge and non-finite argument */
    if hx >= 0x4043687A {
        /* if |x|>=56*ln2 */
        if hx >= 0x40862E42 {
            /* if |x|>=709.78... */
            if hx >= 0x7FF00000 {
                let low = low_word(x);
                if ((hx & 0xFFFFF) | low) != 0 {
                    return x + x; /* NaN */
                } else {
                    return if xsb == 0 { x } else { -1.0 }; /* exp(+-inf)={inf,-1} */
                }
            }
            if x > o_threshold {
                return huge * huge; /* overflow */
            }
        }
        if xsb != 0 {
            /* x < -56*ln2, return -1.0 with inexact */
            if x + tiny < 0.0 {
                /* raise inexact */
                return tiny - one; /* return -1 */
            }
        }
    }

    /* argument reduction */
    if hx > 0x3FD62E42 {
        /* if  |x| > 0.5 ln2 */
        if hx < 0x3FF0A2B2 {
            /* and |x| < 1.5 ln2 */
            if xsb == 0 {
                hi = x - ln2_hi;
                lo = ln2_lo;
                k = 1;
            } else {
                hi = x + ln2_hi;
                lo = -ln2_lo;
                k = -1;
            }
        } else {
            k = (invln2 * x + (if xsb == 0 { 0.5 } else { -0.5 })) as i32;
            t = k as f64;
            hi = x - t * ln2_hi; /* t*ln2_hi is exact here */
            lo = t * ln2_lo;
        }
        x = hi - lo;
        c = (hi - x) - lo;
    } else if hx < 0x3C900000 {
        /* when |x|<2**-54, return x */
        t = huge + x; /* return x with inexact flags when x!=0 */
        return x - (t - (huge + x));
    } else {
        k = 0;
    }

    /* x is now in primary range */
    hfx = 0.5 * x;
    hxs = x * hfx;
    r1 = one + hxs * (Q1 + hxs * (Q2 + hxs * (Q3 + hxs * (Q4 + hxs * Q5))));
    t = 3.0 - r1 * hfx;
    e = hxs * ((r1 - t) / (6.0 - x * t));
    if k == 0 {
        x - (x * e - hxs) /* c is 0 */
    } else {
        twopk = insert_words(0x3FF00000i32.wrapping_add(((k as u32) << 20) as i32), 0); /* 2^k */
        e = x * (e - c) - c;
        e -= hxs;
        if k == -1 {
            return 0.5 * (x - e) - 0.5;
        }
        if k == 1 {
            if x < -0.25 {
                return -2.0 * (e - (x + 0.5));
            } else {
                return one + 2.0 * (x - e);
            }
        }
        if k <= -2 || k > 56 {
            /* suffice to return exp(x)-1 */
            y = one - (e - x);
            // TODO(mvstanton): is this replacement for the hex float
            // sufficient?
            // if (k == 1024) y = y*2.0*0x1p1023;
            if k == 1024 {
                y = y * 2.0 * 8.98846567431158e+307;
            } else {
                y *= twopk;
            }
            return y - one;
        }
        t = one;
        if k < 20 {
            t = set_high_word(t, 0x3FF00000 - (0x200000 >> k)); /* t=1-2^-k */
            y = t - (e - x);
            y *= twopk;
        } else {
            t = set_high_word(t, (0x3FF - k) << 20); /* 2^-k */
            y = x - (e + t);
            y += one;
            y *= twopk;
        }
        y
    }
}

pub fn cbrt(x: f64) -> f64 {
    const B1: u32 = 715094163; /* B1 = (1023-1023/3-0.03306235651)*2**20 */
    const B2: u32 = 696219795; /* B2 = (1023-1023/3-54/3-0.03306235651)*2**20 */

    /* |1/cbrt(x) - p(x)| < 2**-23.5 (~[-7.93e-8, 7.929e-8]). */
    const P0: f64 = 1.87595182427177009643; /* 0x3FFE03E6, 0x0F61E692 */
    const P1: f64 = -1.88497979543377169875; /* 0xBFFE28E0, 0x92F02420 */
    const P2: f64 = 1.621429720105354466140; /* 0x3FF9F160, 0x4A49D6C2 */
    const P3: f64 = -0.758397934778766047437; /* 0xBFE844CB, 0xBEE751D9 */
    const P4: f64 = 0.145996192886612446982; /* 0x3FC2B000, 0xD4E4EDD7 */

    let mut hx: i32;
    let mut r: f64;
    let s: f64;
    let mut t: f64 = 0.0;
    let w: f64;
    let sign: u32;
    let high: u32;
    let low: u32;

    hx = high_word(x);
    low = low_word(x);
    sign = hx as u32 & 0x80000000; /* sign= sign(x) */
    hx = (hx as u32 ^ sign) as i32;
    if hx >= 0x7FF00000 {
        return x + x; /* cbrt(NaN,INF) is itself */
    }

    /*
     * Rough cbrt to 5 bits:
     *    cbrt(2**e*(1+m) ~= 2**(e/3)*(1+(e%3+m)/3)
     * where e is integral and >= 0, m is real and in [0, 1), and "/" and
     * "%" are integer division and modulus with rounding towards minus
     * infinity.  The RHS is always >= the LHS and has a maximum relative
     * error of about 1 in 16.  Adding a bias of -0.03306235651 to the
     * (e%3+m)/3 term reduces the error to about 1 in 32. With the IEEE
     * floating point representation, for finite positive normal values,
     * ordinary integer division of the value in bits magically gives
     * almost exactly the RHS of the above provided we first subtract the
     * exponent bias (1023 for doubles) and later add it back.  We do the
     * subtraction virtually to keep e >= 0 so that ordinary integer
     * division rounds towards minus infinity; this is also efficient.
     */
    if hx < 0x00100000 {
        /* zero or subnormal? */
        if ((hx as u32) | low) == 0 {
            return x; /* cbrt(0) is itself */
        }
        t = set_high_word(t, 0x43500000); /* set t= 2**54 */
        t *= x;
        high = high_word(t) as u32;
        t = insert_words((sign | ((high & 0x7FFFFFFF) / 3).wrapping_add(B2)) as i32, 0);
    } else {
        t = insert_words((sign | (hx as u32 / 3).wrapping_add(B1)) as i32, 0);
    }

    /*
     * New cbrt to 23 bits:
     *    cbrt(x) = t*cbrt(x/t**3) ~= t*P(t**3/x)
     * where P(r) is a polynomial of degree 4 that approximates 1/cbrt(r)
     * to within 2**-23.5 when |r - 1| < 1/10.  The rough approximation
     * has produced t such than |t/cbrt(x) - 1| ~< 1/32, and cubing this
     * gives us bounds for r = t**3/x.
     *
     * Try to optimize for parallel evaluation as in k_tanf.c.
     */
    r = (t * t) * (t / x);
    t *= (P0 + r * (P1 + r * P2)) + ((r * r) * r) * (P3 + r * P4);

    /*
     * Round t away from zero to 23 bits (sloppily except for ensuring that
     * the result is larger in magnitude than cbrt(x) but not much more than
     * 2 23-bit ulps larger).  With rounding towards zero, the error bound
     * would be ~5/6 instead of ~4/6.  With a maximum error of 2 23-bit ulps
     * in the rounded t, the infinite-precision error in the Newton
     * approximation barely affects third digit in the final error
     * 0.667; the error in the rounded t can be up to about 3 23-bit ulps
     * before the final error is larger than 0.667 ulps.
     */
    let mut bits = t.to_bits();
    bits = bits.wrapping_add(0x80000000) & 0xFFFFFFFFC0000000u64;
    t = f64::from_bits(bits);

    /* one step Newton iteration to 53 bits with error < 0.667 ulps */
    s = t * t; /* t*t is exact */
    r = x / s; /* error <= 0.5 ulps; |r| < |t| */
    w = t + t; /* t+t is exact */
    r = (r - t) / (w + r); /* r-t is exact; w+r ~= 3*t */
    t += t * r; /* error <= 0.5 + 0.5/3 + epsilon */

    t
}

/* sin(x)
 * Return sine function of x.
 *
 * kernel function:
 *      __kernel_sin            ... sine function on [-pi/4,pi/4]
 *      __kernel_cos            ... cose function on [-pi/4,pi/4]
 *      __ieee754_rem_pio2      ... argument reduction routine
 *
 * Method.
 *      Let S,C and T denote the sin, cos and tan respectively on
 *      [-PI/4, +PI/4]. Reduce the argument x to y1+y2 = x-k*pi/2
 *      in [-pi/4 , +pi/4], and let n = k mod 4.
 *      We have
 *
 *          n        sin(x)      cos(x)        tan(x)
 *     ----------------------------------------------------------
 *          0          S           C             T
 *          1          C          -S            -1/T
 *          2         -S          -C             T
 *          3         -C           S            -1/T
 *     ----------------------------------------------------------
 *
 * Special cases:
 *      Let trig be any of sin, cos, or tan.
 *      trig(+-INF)  is NaN, with signals;
 *      trig(NaN)    is that NaN;
 *
 * This is the fdlibm_sin body (the V8_USE_LIBM_TRIG_FUNCTIONS=off path).
 */
pub fn sin(x: f64) -> f64 {
    let mut y = [0.0f64; 2];
    let z = 0.0;

    /* High word of x. */
    let mut ix = high_word(x);

    /* |x| ~< pi/4 */
    ix &= 0x7FFFFFFF;
    if ix <= 0x3FE921FB {
        kernel_sin(x, z, 0)
    } else if ix >= 0x7FF00000 {
        /* sin(Inf or NaN) is NaN */
        x - x
    } else {
        /* argument reduction needed */
        let n = rem_pio2(x, &mut y);
        match n & 3 {
            0 => kernel_sin(y[0], y[1], 1),
            1 => kernel_cos(y[0], y[1]),
            2 => -kernel_sin(y[0], y[1], 1),
            _ => -kernel_cos(y[0], y[1]),
        }
    }
}

/* tan(x)
 * Return tangent function of x.
 *
 * kernel function:
 *      __kernel_tan            ... tangent function on [-pi/4,pi/4]
 *      __ieee754_rem_pio2      ... argument reduction routine
 *
 * Special cases:
 *      Let trig be any of sin, cos, or tan.
 *      trig(+-INF)  is NaN, with signals;
 *      trig(NaN)    is that NaN;
 */
pub fn tan(x: f64) -> f64 {
    let mut y = [0.0f64; 2];
    let z = 0.0;

    /* High word of x. */
    let mut ix = high_word(x);

    /* |x| ~< pi/4 */
    ix &= 0x7FFFFFFF;
    if ix <= 0x3FE921FB {
        kernel_tan(x, z, 1)
    } else if ix >= 0x7FF00000 {
        /* tan(Inf or NaN) is NaN */
        x - x /* NaN */
    } else {
        /* argument reduction needed */
        let n = rem_pio2(x, &mut y);
        /* 1 -> n even, -1 -> n odd */
        kernel_tan(y[0], y[1], 1 - ((n & 1) << 1))
    }
}

/*
 * ES6 draft 09-27-13, section 20.2.2.12.
 * Math.cosh
 * Method :
 * mathematically cosh(x) if defined to be (exp(x)+exp(-x))/2
 *      1. Replace x by |x| (cosh(x) = cosh(-x)).
 *      2.
 *                                                      [ exp(x) - 1 ]^2
 *          0        <= x <= ln2/2  :  cosh(x) := 1 + -------------------
 *                                                         2*exp(x)
 *
 *                                                 exp(x) + 1/exp(x)
 *          ln2/2    <= x <= 22     :  cosh(x) := -------------------
 *                                                        2
 *          22       <= x <= lnovft :  cosh(x) := exp(x)/2
 *          lnovft   <= x <= ln2ovft:  cosh(x) := exp(x/2)/2 * exp(x/2)
 *          ln2ovft  <  x           :  cosh(x) := huge*huge (overflow)
 *
 * Special cases:
 *      cosh(x) is |x| if x is +INF, -INF, or NaN.
 *      only cosh(0)=1 is exact for finite x.
 */
pub fn cosh(x: f64) -> f64 {
    const KCOSH_OVERFLOW: f64 = 710.4758600739439;
    const one: f64 = 1.0;
    const half: f64 = 0.5;
    const huge: f64 = 1.0e+300;

    let mut ix: i32;

    /* High word of |x|. */
    ix = high_word(x);
    ix &= 0x7FFFFFFF;

    // |x| in [0,0.5*log2], return 1+expm1(|x|)^2/(2*exp(|x|))
    if ix < 0x3FD62E43 {
        let t = expm1(x.abs());
        let w = one + t;
        // For |x| < 2^-55, cosh(x) = 1
        if ix < 0x3C800000 {
            return w;
        }
        return one + (t * t) / (w + w);
    }

    // |x| in [0.5*log2, 22], return (exp(|x|)+1/exp(|x|)/2
    if ix < 0x40360000 {
        let t = exp(x.abs());
        return half * t + half / t;
    }

    // |x| in [22, log(maxdouble)], return half*exp(|x|)
    if ix < 0x40862E42 {
        return half * exp(x.abs());
    }

    // |x| in [log(maxdouble), overflowthreshold]
    if x.abs() <= KCOSH_OVERFLOW {
        let w = exp(half * x.abs());
        let t = half * w;
        return t * w;
    }

    /* x is INF or NaN */
    if ix >= 0x7FF00000 {
        return x * x;
    }

    // |x| > overflowthreshold.
    huge * huge
}

/*
 * ES2019 Draft 2019-01-02 12.6.4
 * Math.pow & Exponentiation Operator
 *
 * Return X raised to the Yth power
 *
 * Method:
 *     Let x =  2   * (1+f)
 *     1. Compute and return log2(x) in two pieces:
 *        log2(x) = w1 + w2,
 *        where w1 has 53-24 = 29 bit trailing zeros.
 *     2. Perform y*log2(x) = n+y' by simulating muti-precision
 *        arithmetic, where |y'|<=0.5.
 *     3. Return x**y = 2**n*exp(y'*log2)
 *
 * Special cases:
 *     1.  (anything) ** 0  is 1
 *     2.  (anything) ** 1  is itself
 *     3.  (anything) ** NAN is NAN
 *     4.  NAN ** (anything except 0) is NAN
 *     5.  +-(|x| > 1) **  +INF is +INF
 *     6.  +-(|x| > 1) **  -INF is +0
 *     7.  +-(|x| < 1) **  +INF is +0
 *     8.  +-(|x| < 1) **  -INF is +INF
 *     9.  +-1         ** +-INF is NAN
 *     10. +0 ** (+anything except 0, NAN)               is +0
 *     11. -0 ** (+anything except 0, NAN, odd integer)  is +0
 *     12. +0 ** (-anything except 0, NAN)               is +INF
 *     13. -0 ** (-anything except 0, NAN, odd integer)  is +INF
 *     14. -0 ** (odd integer) = -( +0 ** (odd integer) )
 *     15. +INF ** (+anything except 0,NAN) is +INF
 *     16. +INF ** (-anything except 0,NAN) is +0
 *     17. -INF ** (anything)  = -0 ** (-anything)
 *     18. (-anything) ** (integer) is (-1)**(integer)*(+anything**integer)
 *     19. (-anything except 0 and inf) ** (non-integer) is NAN
 *
 * Accuracy:
 *      pow(x,y) returns x**y nearly rounded. In particular,
 *      pow(integer, integer) always returns the correct integer provided it is
 *      representable.
 */
pub fn pow(x: f64, y: f64) -> f64 {
    static bp: [f64; 2] = [1.0, 1.5];
    static dp_h: [f64; 2] = [0.0, 5.84962487220764160156e-01]; // 0x3FE2B803, 0x40000000
    static dp_l: [f64; 2] = [0.0, 1.35003920212974897128e-08]; // 0x3E4CFDEB, 0x43CFD006
    const zero: f64 = 0.0;
    const one: f64 = 1.0;
    const two: f64 = 2.0;
    const two53: f64 = 9007199254740992.0; // 0x43400000, 0x00000000
    const huge: f64 = 1.0e300;
    const tiny: f64 = 1.0e-300;
    // poly coefs for (3/2)*(log(x)-2s-2/3*s**3
    const L1: f64 = 5.99999999999994648725e-01; // 0x3FE33333, 0x33333303
    const L2: f64 = 4.28571428578550184252e-01; // 0x3FDB6DB6, 0xDB6FABFF
    const L3: f64 = 3.33333329818377432918e-01; // 0x3FD55555, 0x518F264D
    const L4: f64 = 2.72728123808534006489e-01; // 0x3FD17460, 0xA91D4101
    const L5: f64 = 2.30660745775561754067e-01; // 0x3FCD864A, 0x93C9DB65
    const L6: f64 = 2.06975017800338417784e-01; // 0x3FCA7E28, 0x4A454EEF
    const P1: f64 = 1.66666666666666019037e-01; // 0x3FC55555, 0x5555553E
    const P2: f64 = -2.77777777770155933842e-03; // 0xBF66C16C, 0x16BEBD93
    const P3: f64 = 6.61375632143793436117e-05; // 0x3F11566A, 0xAF25DE2C
    const P4: f64 = -1.65339022054652515390e-06; // 0xBEBBBD41, 0xC5D26BF1
    const P5: f64 = 4.13813679705723846039e-08; // 0x3E663769, 0x72BEA4D0
    const lg2: f64 = 6.93147180559945286227e-01; // 0x3FE62E42, 0xFEFA39EF
    const lg2_h: f64 = 6.93147182464599609375e-01; // 0x3FE62E43, 0x00000000
    const lg2_l: f64 = -1.90465429995776804525e-09; // 0xBE205C61, 0x0CA86C39
    const ovt: f64 = 8.0085662595372944372e-0017; // -(1024-log2(ovfl+.5ulp))
    const cp: f64 = 9.61796693925975554329e-01; // 0x3FEEC709, 0xDC3A03FD =2/(3ln2)
    const cp_h: f64 = 9.61796700954437255859e-01; // 0x3FEEC709, 0xE0000000 =(float)cp
    const cp_l: f64 = -7.02846165095275826516e-09; // 0xBE3E2FE0, 0x145B01F5 =tail cp_h
    const ivln2: f64 = 1.44269504088896338700e+00; // 0x3FF71547, 0x652B82FE =1/ln2
    const ivln2_h: f64 = 1.44269502162933349609e+00; // 0x3FF71547, 0x60000000 =24b 1/ln2
    const ivln2_l: f64 = 1.92596299112661746887e-08; // 0x3E54AE0B, 0xF85DDF44 =1/ln2 tail

    let mut z: f64;
    let mut ax: f64;
    let z_h: f64;
    let z_l: f64;
    let mut p_h: f64;
    let mut p_l: f64;
    let y1: f64;
    let t1: f64;
    let t2: f64;
    let r: f64;
    let s: f64;
    let mut t: f64;
    let mut u: f64;
    let mut v: f64;
    let w: f64;
    let mut i: i32;
    let mut j: i32;
    let mut k: i32;
    let yisint: i32;
    let mut n: i32;

    let hx = high_word(x);
    let lx = low_word(x);
    let hy = high_word(y);
    let ly = low_word(y);
    let mut ix = hx & 0x7fffffff;
    let iy = hy & 0x7fffffff;

    /* y==zero: x**0 = 1 */
    if ((iy as u32) | ly) == 0 {
        return one;
    }

    /* +-NaN return x+y */
    if ix > 0x7ff00000
        || ((ix == 0x7ff00000) && (lx != 0))
        || iy > 0x7ff00000
        || ((iy == 0x7ff00000) && (ly != 0))
    {
        return x + y;
    }

    /* determine if y is an odd int when x < 0
     * yisint = 0 ... y is not an integer
     * yisint = 1 ... y is an odd int
     * yisint = 2 ... y is an even int
     */
    let mut yi = 0;
    if hx < 0 {
        if iy >= 0x43400000 {
            yi = 2; /* even integer y */
        } else if iy >= 0x3ff00000 {
            k = (iy >> 20) - 0x3ff; /* exponent */
            if k > 20 {
                j = (ly >> (52 - k)) as i32;
                if (j << (52 - k)) == ly as i32 {
                    yi = 2 - (j & 1);
                }
            } else if ly == 0 {
                j = iy >> (20 - k);
                if (j << (20 - k)) == iy {
                    yi = 2 - (j & 1);
                }
            }
        }
    }
    yisint = yi;

    /* special value of y */
    if ly == 0 {
        if iy == 0x7ff00000 {
            /* y is +-inf */
            if (((ix - 0x3ff00000) as u32) | lx) == 0 {
                return y - y; /* inf**+-1 is NaN */
            } else if ix >= 0x3ff00000 {
                /* (|x|>1)**+-inf = inf,0 */
                return if hy >= 0 { y } else { zero };
            } else {
                /* (|x|<1)**-,+inf = inf,0 */
                return if hy < 0 { -y } else { zero };
            }
        }
        if iy == 0x3ff00000 {
            /* y is  +-1 */
            if hy < 0 {
                return one / x;
            } else {
                return x;
            }
        }
        if hy == 0x40000000 {
            return x * x; /* y is  2 */
        }
        if hy == 0x3fe00000 {
            /* y is  0.5 */
            if hx >= 0 {
                /* x >= +0 */
                return x.sqrt();
            }
        }
    }

    ax = x.abs();
    /* special value of x */
    if lx == 0 && (ix == 0x7ff00000 || ix == 0 || ix == 0x3ff00000) {
        z = ax; /*x is +-0,+-inf,+-1*/
        if hy < 0 {
            z = one / z; /* z = (1/|x|) */
        }
        if hx < 0 {
            if ((ix - 0x3ff00000) | yisint) == 0 {
                /* (-1)**non-int is NaN */
                z = f64::NAN;
            } else if yisint == 1 {
                z = -z; /* (x<0)**odd = -(|x|**odd) */
            }
        }
        return z;
    }

    n = (hx >> 31) + 1;

    /* (x<0)**(non-int) is NaN */
    if (n | yisint) == 0 {
        return f64::NAN;
    }

    s = if (n | (yisint - 1)) == 0 { -one } else { one }; /* (-ve)**(odd int) */

    /* |y| is huge */
    if iy > 0x41e00000 {
        /* if |y| > 2**31 */
        if iy > 0x43f00000 {
            /* if |y| > 2**64, must o/uflow */
            if ix <= 0x3fefffff {
                return if hy < 0 { huge * huge } else { tiny * tiny };
            }
            if ix >= 0x3ff00000 {
                return if hy > 0 { huge * huge } else { tiny * tiny };
            }
        }
        /* over/underflow if x is not close to one */
        if ix < 0x3fefffff {
            return if hy < 0 { s * huge * huge } else { s * tiny * tiny };
        }
        if ix > 0x3ff00000 {
            return if hy > 0 { s * huge * huge } else { s * tiny * tiny };
        }
        /* now |1-x| is tiny <= 2**-20, suffice to compute
        log(x) by x-x^2/2+x^3/3-x^4/4 */
        t = ax - one; /* t has 20 trailing zeros */
        w = (t * t) * (0.5 - t * (0.3333333333333333333333 - t * 0.25));
        u = ivln2_h * t; /* ivln2_h has 21 sig. bits */
        v = t * ivln2_l - w * ivln2;
        let t1_ = u + v;
        t1 = set_low_word(t1_, 0);
        t2 = v - (t1 - u);
    } else {
        let ss: f64;
        let mut s2: f64;
        let s_h: f64;
        let s_l: f64;
        let mut t_h: f64;
        let t_l: f64;
        n = 0;
        /* take care subnormal number */
        if ix < 0x00100000 {
            ax *= two53;
            n -= 53;
            ix = high_word(ax);
        }
        n += (ix >> 20) - 0x3ff;
        j = ix & 0x000fffff;
        /* determine interval */
        ix = j | 0x3ff00000; /* normalize ix */
        if j <= 0x3988E {
            k = 0; /* |x|<sqrt(3/2) */
        } else if j < 0xBB67A {
            k = 1; /* |x|<sqrt(3)   */
        } else {
            k = 0;
            n += 1;
            ix -= 0x00100000;
        }
        ax = set_high_word(ax, ix);

        /* compute ss = s_h+s_l = (x-1)/(x+1) or (x-1.5)/(x+1.5) */
        u = ax - bp[k as usize]; /* bp[0]=1.0, bp[1]=1.5 */
        v = one / (ax + bp[k as usize]);
        ss = u * v;
        s_h = set_low_word(ss, 0);
        /* t_h=ax+bp[k] High */
        t_h = zero;
        t_h = set_high_word(t_h, ((ix >> 1) | 0x20000000) + 0x00080000 + (k << 18));
        t_l = ax - (t_h - bp[k as usize]);
        s_l = v * ((u - s_h * t_h) - s_h * t_l);
        /* compute log(ax) */
        s2 = ss * ss;
        let mut r_ = s2 * s2 * (L1 + s2 * (L2 + s2 * (L3 + s2 * (L4 + s2 * (L5 + s2 * L6)))));
        r_ += s_l * (s_h + ss);
        s2 = s_h * s_h;
        t_h = 3.0 + s2 + r_;
        t_h = set_low_word(t_h, 0);
        let t_l2 = r_ - ((t_h - 3.0) - s2);
        /* u+v = ss*(1+...) */
        u = s_h * t_h;
        v = s_l * t_h + t_l2 * ss;
        /* 2/(3log2)*(ss+...) */
        p_h = u + v;
        p_h = set_low_word(p_h, 0);
        p_l = v - (p_h - u);
        z_h = cp_h * p_h; /* cp_h+cp_l = 2/(3*log2) */
        z_l = cp_l * p_h + p_l * cp + dp_l[k as usize];
        /* log2(ax) = (ss+..)*2/(3*log2) = n + dp_h + z_h + z_l */
        t = n as f64;
        let t1_ = ((z_h + z_l) + dp_h[k as usize]) + t;
        t1 = set_low_word(t1_, 0);
        t2 = z_l - (((t1 - t) - dp_h[k as usize]) - z_h);
    }

    /* split up y into y1+y2 and compute (y1+y2)*(t1+t2) */
    y1 = set_low_word(y, 0);
    p_l = (y - y1) * t1 + y * t2;
    p_h = y1 * t1;
    z = p_l + p_h;
    j = high_word(z);
    i = low_word(z) as i32;
    if j >= 0x40900000 {
        /* z >= 1024 */
        if (j.wrapping_sub(0x40900000) | i) != 0 {
            /* if z > 1024 */
            return s * huge * huge; /* overflow */
        } else if p_l + ovt > z - p_h {
            return s * huge * huge; /* overflow */
        }
    } else if (j & 0x7fffffff) >= 0x4090cc00 {
        /* z <= -1075 */
        if ((j as u32).wrapping_sub(0xc090cc00) | i as u32) != 0 {
            /* z < -1075 */
            return s * tiny * tiny; /* underflow */
        } else if p_l <= z - p_h {
            return s * tiny * tiny; /* underflow */
        }
    }
    /*
     * compute 2**(p_h+p_l)
     */
    i = j & 0x7fffffff;
    k = (i >> 20) - 0x3ff;
    n = 0;
    if i > 0x3fe00000 {
        /* if |z| > 0.5, set n = [z+0.5] */
        n = j.wrapping_add(0x00100000 >> (k + 1));
        k = ((n & 0x7fffffff) >> 20) - 0x3ff; /* new k for n */
        t = zero;
        t = set_high_word(t, n & !(0x000fffff >> k));
        n = ((n & 0x000fffff) | 0x00100000) >> (20 - k);
        if j < 0 {
            n = -n;
        }
        p_h -= t;
    }
    t = p_l + p_h;
    t = set_low_word(t, 0);
    u = t * lg2_h;
    v = (p_l - (t - p_h)) * lg2 + t * lg2_l;
    z = u + v;
    let w = v - (z - u);
    t = z * z;
    let t1 = z - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    r = (z * t1) / ((t1 - two) - (w + z * w));
    z = one - (r - z);
    j = high_word(z);
    j = j.wrapping_add(((n as u32) << 20) as i32);
    if (j >> 20) <= 0 {
        z = scalbn(z, n); /* subnormal output */
    } else {
        let tmp = high_word(z);
        z = set_high_word(z, tmp.wrapping_add(((n as u32) << 20) as i32));
    }
    s * z
}

/*
 * ES6 draft 09-27-13, section 20.2.2.30.
 * Math.sinh
 * Method :
 * mathematically sinh(x) if defined to be (exp(x)-exp(-x))/2
 *      1. Replace x by |x| (sinh(-x) = -sinh(x)).
 *      2.
 *                                                  E + E/(E+1)
 *          0        <= x <= 22     :  sinh(x) := --------------, E=expm1(x)
 *                                                      2
 *
 *          22       <= x <= lnovft :  sinh(x) := exp(x)/2
 *          lnovft   <= x <= ln2ovft:  sinh(x) := exp(x/2)/2 * exp(x/2)
 *          ln2ovft  <  x           :  sinh(x) := x*shuge (overflow)
 *
 * Special cases:
 *      sinh(x) is |x| if x is +Infinity, -Infinity, or NaN.
 *      only sinh(0)=0 is exact for finite x.
 */
pub fn sinh(x: f64) -> f64 {
    const KSINH_OVERFLOW: f64 = 710.4758600739439;
    const TWO_M28: f64 = 3.725290298461914e-9; // 2^-28, empty lower half
    const LOG_MAXD: f64 = 709.7822265625; // 0x40862E42 00000000, empty lower half
    const shuge: f64 = 1.0e307;

    let h = if x < 0.0 { -0.5 } else { 0.5 };
    // |x| in [0, 22]. return sign(x)*0.5*(E+E/(E+1))
    let ax = x.abs();
    if ax < 22.0 {
        // For |x| < 2^-28, sinh(x) = x
        if ax < TWO_M28 {
            return x;
        }
        let t = expm1(ax);
        if ax < 1.0 {
            return h * (2.0 * t - t * t / (t + 1.0));
        }
        return h * (t + t / (t + 1.0));
    }
    // |x| in [22, log(maxdouble)], return 0.5 * exp(|x|)
    if ax < LOG_MAXD {
        return h * exp(ax);
    }
    // |x| in [log(maxdouble), overflowthreshold]
    // overflowthreshold = 710.4758600739426
    if ax <= KSINH_OVERFLOW {
        let w = exp(0.5 * ax);
        let t = h * w;
        return t * w;
    }
    // |x| > overflowthreshold or is NaN.
    // Return Infinity of the appropriate sign or NaN.
    x * shuge
}

/* Tanh(x)
 * Return the Hyperbolic Tangent of x
 *
 * Method :
 *                                 x    -x
 *                                e  - e
 *  0. tanh(x) is defined to be -----------
 *                                 x    -x
 *                                e  + e
 *  1. reduce x to non-negative by tanh(-x) = -tanh(x).
 *  2.  0      <= x <  2**-28 : tanh(x) := x with inexact if x != 0
 *                                          -t
 *      2**-28 <= x <  1      : tanh(x) := -----; t = expm1(-2x)
 *                                         t + 2
 *                                               2
 *      1      <= x <  22     : tanh(x) := 1 - -----; t = expm1(2x)
 *                                             t + 2
 *      22     <= x <= INF    : tanh(x) := 1.
 *
 * Special cases:
 *      tanh(NaN) is NaN;
 *      only tanh(0)=0 is exact for finite argument.
 */
pub fn tanh(x: f64) -> f64 {
    const tiny: f64 = 1.0e-300;
    const one: f64 = 1.0;
    const two: f64 = 2.0;
    const huge: f64 = 1.0e300;
    let t: f64;
    let z: f64;

    let jx = high_word(x);
    let ix = jx & 0x7FFFFFFF;

    /* x is INF or NaN */
    if ix >= 0x7FF00000 {
        if jx >= 0 {
            return one / x + one; /* tanh(+-inf)=+-1 */
        } else {
            return one / x - one; /* tanh(NaN) = NaN */
        }
    }

    /* |x| < 22 */
    if ix < 0x40360000 {
        /* |x|<22 */
        if ix < 0x3E300000 {
            /* |x|<2**-28 */
            if huge + x > one {
                return x; /* tanh(tiny) = tiny with inexact */
            }
        }
        if ix >= 0x3FF00000 {
            /* |x|>=1  */
            t = expm1(two * x.abs());
            z = one - two / (t + two);
        } else {
            t = expm1(-two * x.abs());
            z = -t / (t + two);
        }
        /* |x| >= 22, return +-1 */
    } else {
        z = one - tiny; /* raise inexact flag */
    }
    if jx >= 0 {
        z
    } else {
        -z
    }
}
