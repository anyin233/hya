use std::io::Read;

#[cfg(feature = "bzip2")]
use bzip2::read::BzDecoder;
#[cfg(feature = "deflate")]
use flate2::bufread::DeflateDecoder;
use lzma_rust2::{
    Lzma2Reader, Lzma2ReaderMt, LzmaReader,
    filter::{bcj::BcjReader, delta::DeltaReader},
    lzma_get_memory_usage_by_props, lzma2_get_memory_usage,
};
#[cfg(feature = "ppmd")]
use ppmd_rust::{
    PPMD7_MAX_MEM_SIZE, PPMD7_MAX_ORDER, PPMD7_MIN_MEM_SIZE, PPMD7_MIN_ORDER, Ppmd7Decoder,
};

#[cfg(feature = "brotli")]
use crate::codec::brotli::BrotliDecoder;
#[cfg(feature = "lz4")]
use crate::codec::lz4::Lz4Decoder;
#[cfg(feature = "aes256")]
use crate::encryption::Aes256Sha256Decoder;
use crate::{ByteReader, Password, archive::EncoderMethod, block::Coder, error::Error};

pub enum Decoder<R: Read> {
    Copy(R),
    Lzma(Box<LzmaReader<R>>),
    Lzma2(Box<Lzma2Reader<R>>),
    Lzma2Mt(Box<Lzma2ReaderMt<R>>),
    #[cfg(feature = "ppmd")]
    Ppmd(Box<Ppmd7Decoder<R>>),
    Bcj(BcjReader<R>),
    Delta(DeltaReader<R>),
    #[cfg(feature = "brotli")]
    Brotli(Box<BrotliDecoder<R>>),
    #[cfg(feature = "bzip2")]
    Bzip2(BzDecoder<R>),
    #[cfg(feature = "deflate")]
    Deflate(DeflateDecoder<std::io::BufReader<R>>),
    #[cfg(feature = "lz4")]
    Lz4(Lz4Decoder<R>),
    #[cfg(feature = "zstd")]
    Zstd(zstd::Decoder<'static, std::io::BufReader<R>>),
    #[cfg(feature = "aes256")]
    Aes256Sha256(Box<Aes256Sha256Decoder<R>>),
}

impl<R: Read> Read for Decoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Decoder::Copy(r) => r.read(buf),
            Decoder::Lzma(r) => r.read(buf),
            Decoder::Lzma2(r) => r.read(buf),
            Decoder::Lzma2Mt(r) => r.read(buf),
            #[cfg(feature = "ppmd")]
            Decoder::Ppmd(r) => r.read(buf),
            Decoder::Bcj(r) => r.read(buf),
            Decoder::Delta(r) => r.read(buf),
            #[cfg(feature = "brotli")]
            Decoder::Brotli(r) => r.read(buf),
            #[cfg(feature = "bzip2")]
            Decoder::Bzip2(r) => r.read(buf),
            #[cfg(feature = "deflate")]
            Decoder::Deflate(r) => r.read(buf),
            #[cfg(feature = "lz4")]
            Decoder::Lz4(r) => r.read(buf),
            #[cfg(feature = "zstd")]
            Decoder::Zstd(r) => r.read(buf),
            #[cfg(feature = "aes256")]
            Decoder::Aes256Sha256(r) => r.read(buf),
        }
    }
}

pub fn add_decoder<I: Read>(
    input: I,
    uncompressed_len: usize,
    coder: &Coder,
    #[allow(unused)] password: &Password,
    max_memory_bytes: usize,
    threads: u32,
    strict: bool,
) -> Result<Decoder<I>, Error> {
    if strict && !strict_coder_id_is_allowed(coder.encoder_method_id()) {
        return Err(Error::UnsupportedCompressionMethod(format!(
            "{:?}",
            coder.encoder_method_id()
        )));
    }
    let method = EncoderMethod::by_id(coder.encoder_method_id());
    let method = if let Some(m) = method {
        m
    } else {
        return Err(Error::UnsupportedCompressionMethod(format!(
            "{:?}",
            coder.encoder_method_id()
        )));
    };
    match method.id() {
        EncoderMethod::ID_COPY => Ok(Decoder::Copy(input)),
        EncoderMethod::ID_LZMA => {
            if strict && coder.properties.len() != 5 {
                return Err(Error::Other(
                    "LZMA properties must be exactly five bytes".into(),
                ));
            }
            let dict_size = get_lzma_dic_size(coder)?;
            let dictionary_bytes = usize::try_from(dict_size)
                .map_err(|_| Error::other("LZMA dictionary does not fit usize"))?;
            if dictionary_bytes > max_memory_bytes {
                return Err(Error::MaxMemLimited {
                    max_bytes: max_memory_bytes,
                    actual_bytes: dictionary_bytes,
                });
            }
            let props = coder.properties[0];
            let memory_kib = lzma_get_memory_usage_by_props(dict_size, props)
                .map_err(|error| Error::bad_password(error, !password.is_empty()))?;
            let memory_bytes = kib_to_bytes(memory_kib)?;
            if memory_bytes > max_memory_bytes {
                return Err(Error::MaxMemLimited {
                    max_bytes: max_memory_bytes,
                    actual_bytes: memory_bytes,
                });
            }
            let lz =
                LzmaReader::new_with_props(input, uncompressed_len as _, props, dict_size, None)
                    .map_err(|e| Error::bad_password(e, !password.is_empty()))?;
            Ok(Decoder::Lzma(Box::new(lz)))
        }
        EncoderMethod::ID_LZMA2 => {
            if strict && coder.properties.len() != 1 {
                return Err(Error::Other(
                    "LZMA2 properties must be exactly one byte".into(),
                ));
            }
            let dic_size = get_lzma2_dic_size(coder)?;
            let dictionary_bytes = usize::try_from(dic_size)
                .map_err(|_| Error::other("LZMA2 dictionary does not fit usize"))?;
            if dictionary_bytes > max_memory_bytes {
                return Err(Error::MaxMemLimited {
                    max_bytes: max_memory_bytes,
                    actual_bytes: dictionary_bytes,
                });
            }
            let mem_size = kib_to_bytes(lzma2_get_memory_usage(dic_size))?;
            if mem_size > max_memory_bytes {
                return Err(Error::MaxMemLimited {
                    max_bytes: max_memory_bytes,
                    actual_bytes: mem_size,
                });
            }

            let lz = if threads < 2 {
                Decoder::Lzma2(Box::new(Lzma2Reader::new(input, dic_size, None)))
            } else {
                Decoder::Lzma2Mt(Box::new(Lzma2ReaderMt::new(input, dic_size, None, threads)))
            };

            Ok(lz)
        }
        #[cfg(feature = "ppmd")]
        EncoderMethod::ID_PPMD => {
            let (order, memory_size) = get_ppmd_order_memory_size(coder, max_memory_bytes)?;
            let ppmd = Ppmd7Decoder::new(input, order, memory_size)
                .map_err(|err| Error::other(err.to_string()))?;
            Ok(Decoder::Ppmd(Box::new(ppmd)))
        }
        #[cfg(feature = "brotli")]
        EncoderMethod::ID_BROTLI => {
            let de = BrotliDecoder::new(input, 4096)?;
            Ok(Decoder::Brotli(Box::new(de)))
        }
        #[cfg(feature = "bzip2")]
        EncoderMethod::ID_BZIP2 => {
            let de = BzDecoder::new(input);
            Ok(Decoder::Bzip2(de))
        }
        #[cfg(feature = "deflate")]
        EncoderMethod::ID_DEFLATE => {
            let buf_read = std::io::BufReader::new(input);
            let de = DeflateDecoder::new(buf_read);
            Ok(Decoder::Deflate(de))
        }
        #[cfg(feature = "lz4")]
        EncoderMethod::ID_LZ4 => {
            let de = Lz4Decoder::new(input)?;
            Ok(Decoder::Lz4(de))
        }
        #[cfg(feature = "zstd")]
        EncoderMethod::ID_ZSTD => {
            let zs = zstd::Decoder::new(input)?;
            Ok(Decoder::Zstd(zs))
        }
        EncoderMethod::ID_BCJ_X86 => {
            let de = BcjReader::new_x86(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_ARM => {
            let de = BcjReader::new_arm(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_ARM64 => {
            let de = BcjReader::new_arm64(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_ARM_THUMB => {
            let de = BcjReader::new_arm_thumb(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_PPC => {
            let de = BcjReader::new_ppc(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_IA64 => {
            let de = BcjReader::new_ia64(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_SPARC => {
            let de = BcjReader::new_sparc(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_BCJ_RISCV => {
            let de = BcjReader::new_riscv(input, 0);
            Ok(Decoder::Bcj(de))
        }
        EncoderMethod::ID_DELTA => {
            let d = if coder.properties.is_empty() {
                1
            } else {
                coder.properties[0].wrapping_add(1)
            };
            let de = DeltaReader::new(input, d as usize);
            Ok(Decoder::Delta(de))
        }
        #[cfg(feature = "aes256")]
        EncoderMethod::ID_AES256_SHA256 => {
            if password.is_empty() {
                return Err(Error::PasswordRequired);
            }
            let de = Aes256Sha256Decoder::new(input, &coder.properties, password)?;
            Ok(Decoder::Aes256Sha256(Box::new(de)))
        }
        _ => Err(Error::UnsupportedCompressionMethod(
            method.name().to_string(),
        )),
    }
}

#[cfg(feature = "ppmd")]
fn get_ppmd_order_memory_size(coder: &Coder, max_memory_bytes: usize) -> Result<(u32, u32), Error> {
    if coder.properties.len() < 5 {
        return Err(Error::other("PPMD properties too short"));
    }
    let order = coder.properties[0] as u32;
    let memory_size = u32::from_le_bytes([
        coder.properties[1],
        coder.properties[2],
        coder.properties[3],
        coder.properties[4],
    ]);

    if order < PPMD7_MIN_ORDER {
        return Err(Error::other("PPMD order smaller than PPMD7_MIN_ORDER"));
    }

    if order > PPMD7_MAX_ORDER {
        return Err(Error::other("PPMD order larger than PPMD7_MAX_ORDER"));
    }

    if memory_size < PPMD7_MIN_MEM_SIZE {
        return Err(Error::other(
            "PPMD memory size smaller than PPMD7_MIN_MEM_SIZE",
        ));
    }

    if memory_size > PPMD7_MAX_MEM_SIZE {
        return Err(Error::other(
            "PPMD memory size larger than PPMD7_MAX_MEM_SIZE",
        ));
    }

    if memory_size as usize > max_memory_bytes {
        return Err(Error::MaxMemLimited {
            max_bytes: max_memory_bytes,
            actual_bytes: memory_size as usize,
        });
    }

    Ok((order, memory_size))
}

fn get_lzma2_dic_size(coder: &Coder) -> Result<u32, Error> {
    if coder.properties.is_empty() {
        return Err(Error::other("LZMA2 properties too short"));
    }
    let dict_size_bits = 0xFF & coder.properties[0] as u32;
    if (dict_size_bits & (!0x3F)) != 0 {
        return Err(Error::other("Unsupported LZMA2 property bits"));
    }
    if dict_size_bits > 40 {
        return Err(Error::other("Dictionary larger than 4GiB maximum size"));
    }
    if dict_size_bits == 40 {
        return Ok(0xFFFFFFFF);
    }
    let size = (2 | (dict_size_bits & 0x1)) << (dict_size_bits / 2 + 11);
    Ok(size)
}

fn get_lzma_dic_size(coder: &Coder) -> Result<u32, Error> {
    if coder.properties.len() < 5 {
        return Err(Error::other("LZMA properties too short"));
    }
    let mut props = &coder.properties[1..5];
    props.read_u32().map_err(Error::from)
}

fn kib_to_bytes(memory_kib: u32) -> Result<usize, Error> {
    usize::try_from(memory_kib)
        .map_err(|_| Error::other("decoder memory requirement does not fit usize"))?
        .checked_mul(1024)
        .ok_or_else(|| Error::other("decoder memory requirement overflows usize"))
}

fn strict_coder_id_is_allowed(id: &[u8]) -> bool {
    id == EncoderMethod::ID_COPY || id == EncoderMethod::ID_LZMA || id == EncoderMethod::ID_LZMA2
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn strict_lzma2_memory_limit_is_measured_in_bytes() {
        const MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;

        let mut coder = Coder::default();
        coder.id_size = EncoderMethod::ID_LZMA2.len();
        coder
            .decompression_method_id_mut()
            .copy_from_slice(EncoderMethod::ID_LZMA2);
        coder.properties = vec![28];

        let result = add_decoder(
            Cursor::new(Vec::<u8>::new()),
            0,
            &coder,
            &Password::empty(),
            MAX_MEMORY_BYTES,
            1,
            true,
        );

        assert!(matches!(result, Err(Error::MaxMemLimited { .. })));
    }

    #[test]
    fn strict_lzma_memory_limit_accounts_for_decoder_overhead() {
        const MAX_MEMORY_BYTES: usize = 1024 * 1024;

        let mut coder = Coder::default();
        coder.id_size = EncoderMethod::ID_LZMA.len();
        coder
            .decompression_method_id_mut()
            .copy_from_slice(EncoderMethod::ID_LZMA);
        coder.properties = vec![0x5d, 0x00, 0x00, 0x10, 0x00];

        let result = add_decoder(
            Cursor::new(Vec::<u8>::new()),
            0,
            &coder,
            &Password::empty(),
            MAX_MEMORY_BYTES,
            1,
            true,
        );

        assert!(matches!(result, Err(Error::MaxMemLimited { .. })));
    }
}
