// Hound -- A wav encoding and decoding library in Rust
// Copyright (C) 2015 Ruud van Asseldonk
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// A copy of the License has been included in the root of the repository.
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::fs;
use std::io;
use std::io::Write;
use std::path;
use super::{Error, Result, Sample, WavSpec};

/// Extends the functionality of `io::Write` with additional methods.
///
/// The methods may be used on any type that implements `io::Write`.
pub trait WriteExt: io::Write {
    /// Writes an unsigned 8-bit integer.
    fn write_u8(&mut self, x: u8) -> io::Result<()>;

    /// Writes a signed 16-bit integer in little endian format.
    fn write_le_i16(&mut self, x: i16) -> io::Result<()>;

    /// Writes an unsigned 16-bit integer in little endian format.
    fn write_le_u16(&mut self, x: u16) -> io::Result<()>;

    /// Writes a signed 24-bit integer in little endian format.
    ///
    /// The most significant byte of the `i32` is ignored.
    fn write_le_i24(&mut self, x: i32) -> io::Result<()>;

    /// Writes an unsigned 24-bit integer in little endian format.
    ///
    /// The most significant byte of the `u32` is ignored.
    fn write_le_u24(&mut self, x: u32) -> io::Result<()>;

    /// Writes a signed 32-bit integer in little endian format.
    fn write_le_i32(&mut self, x: i32) -> io::Result<()>;

    /// Writes an unsigned 32-bit integer in little endian format.
    fn write_le_u32(&mut self, x: u32) -> io::Result<()>;
}

impl<W> WriteExt for W where W: io::Write {
    fn write_u8(&mut self, x: u8) -> io::Result<()> {
        let buf = [x];
        self.write_all(&buf)
    }

    fn write_le_i16(&mut self, x: i16) -> io::Result<()> {
        self.write_le_u16(x as u16)
    }

    fn write_le_u16(&mut self, x: u16) -> io::Result<()> {
        let mut buf = [0u8; 2];
        buf[0] = (x & 0xff) as u8;
        buf[1] = (x >> 8) as u8;
        self.write_all(&buf)
    }

    fn write_le_i24(&mut self, x: i32) -> io::Result<()> {
        self.write_le_u24(x as u32)
    }

    fn write_le_u24(&mut self, x: u32) -> io::Result<()> {
        let mut buf = [0u8; 3];
        buf[0] = ((x >> 00) & 0xff) as u8;
        buf[1] = ((x >> 08) & 0xff) as u8;
        buf[2] = ((x >> 16) & 0xff) as u8;
        self.write_all(&buf)
    }

    fn write_le_i32(&mut self, x: i32) -> io::Result<()> {
        self.write_le_u32(x as u32)
    }

    fn write_le_u32(&mut self, x: u32) -> io::Result<()> {
        let mut buf = [0u8; 4];
        buf[0] = ((x >> 00) & 0xff) as u8;
        buf[1] = ((x >> 08) & 0xff) as u8;
        buf[2] = ((x >> 16) & 0xff) as u8;
        buf[3] = ((x >> 24) & 0xff) as u8;
        self.write_all(&buf)
    }
}

/// Generates a bitmask with `channels` ones in the least significant bits.
fn channel_mask(channels: u16) -> u32 {
    (0 .. channels).map(|c| 1 << c).fold(0, |a, c| a | c)
}

#[test]
fn verify_channel_mask()
{
    assert_eq!(channel_mask(0), 0);
    assert_eq!(channel_mask(1), 1);
    assert_eq!(channel_mask(2), 3);
    assert_eq!(channel_mask(3), 7);
    assert_eq!(channel_mask(4), 15);
}

/// A writer that accepts samples and writes the WAVE format.
///
/// The writer needs a `WavSpec` that describes the audio properties. Then
/// samples can be written with `write_sample`. Channel data is interleaved.
/// The number of samples written must be a multiple of the number of channels.
/// After all samples have been written, the file must be finalized. This can
/// be done by calling `finalize`. If `finalize` is not called, the file will
/// be finalized upon drop. However, finalization may fail, and without calling
/// `finalize`, such a failure cannot be observed.
pub struct WavWriter<W> where W: io::Write + io::Seek {
    /// Specifies properties of the audio data.
    spec: WavSpec,

    /// The (container) bytes per sample. This is the bit rate / 8 rounded up.
    bytes_per_sample: u16,

    /// Whether the header has been written already.
    wrote_header: bool,

    /// The writer that will be written to.
    writer: io::BufWriter<W>,

    /// The number of bytes written to the data section.
    ///
    /// This is an `u32` because WAVE cannot accomodate more data.
    data_bytes_written: u32,

    /// Whether `finalize_internal` has been called.
    finalized: bool
}

impl<W> WavWriter<W> where W: io::Write + io::Seek {
    /// Creates a writer that writes the WAVE format to the underlying writer.
    ///
    /// The underlying writer is assumed to be at offset 0. `WavWriter` employs
    /// buffering internally to avoid too many `write` calls to the underlying
    /// writer.
    pub fn new(writer: W, spec: WavSpec) -> WavWriter<W> {
        WavWriter {
            spec: spec,
            bytes_per_sample: (spec.bits_per_sample as f32 / 8.0).ceil() as u16,
            wrote_header: false,
            writer: io::BufWriter::new(writer),
            data_bytes_written: 0,
            finalized: false
        }
    }

    /// Writes the RIFF WAVE header
    fn write_header(&mut self) -> io::Result<()> {
        // Useful links:
        // https://msdn.microsoft.com/en-us/library/ms713462.aspx
        // https://msdn.microsoft.com/en-us/library/ms713497.aspx

        let mut header = [0u8; 68];
        let spec = &self.spec;

        // Write the header in-memory first.
        {
            let mut buffer: io::Cursor<&mut [u8]> = io::Cursor::new(&mut header);
            try!(buffer.write_all("RIFF".as_bytes()));

            // Skip 4 bytes that will be filled with the file size afterwards.
            try!(buffer.write_le_u32(0));

            try!(buffer.write_all("WAVE".as_bytes()));
            try!(buffer.write_all("fmt ".as_bytes()));
            try!(buffer.write_le_u32(40)); // Size of the WAVE header chunk.

            // The following is based on the WAVEFORMATEXTENSIBLE struct as
            // documented on MSDN.

            // The field wFormatTag, value 1 means WAVE_FORMAT_PCM, but we use
            // the slightly more sophisticated WAVE_FORMAT_EXTENSIBLE.
            try!(buffer.write_le_u16(0xfffe));
            // The field nChannels.
            try!(buffer.write_le_u16(spec.channels));
            // The field nSamplesPerSec.
            try!(buffer.write_le_u32(spec.sample_rate));
            let bytes_per_sec = spec.sample_rate
                              * self.bytes_per_sample as u32
                              * spec.channels as u32;
            // The field nAvgBytesPerSec;
            try!(buffer.write_le_u32(bytes_per_sec));
            // The field nBlockAlign. Block align * sample rate = bytes per sec.
            try!(buffer.write_le_u16((bytes_per_sec / spec.sample_rate) as u16));
            // The field wBitsPerSample. This is actually the size of the
            // container, so this is a multiple of 8.
            try!(buffer.write_le_u16(self.bytes_per_sample as u16 * 8));
            // The field cbSize, the number of remaining bytes in the struct.
            try!(buffer.write_le_u16(22));
            // The field wValidBitsPerSample, the real number of bits per sample.
            try!(buffer.write_le_u16(self.spec.bits_per_sample));
            // The field dwChannelMask.
            // TODO: add the option to specify the channel mask. For now, use
            // the default assignment.
            try!(buffer.write_le_u32(channel_mask(self.spec.channels)));
            // The field SubFormat. We use PCM audio with integer samples.
            try!(buffer.write_all(&super::KSDATAFORMAT_SUBTYPE_PCM));

            // So far the "fmt " section, now comes the "data" section. We will
            // only write the header here, actual data are the samples. The
            // number of bytes that this will take is not known at this point.
            // The 0 will be overwritten later.
            try!(buffer.write_all("data".as_bytes()));
            try!(buffer.write_le_u32(0));
        }

        // Then write the entire header at once.
        try!(self.writer.write_all(&header));

        Ok(())
    }

    /// Writes a single sample for one channel.
    ///
    /// WAVE interleaves channel data, so the channel that this writes the
    /// sample to depends on previous writes. This will return an error if the
    /// sample does not fit in the number of bits specified in the `WavSpec`.
    pub fn write_sample<S: Sample>(&mut self, sample: S) -> Result<()> {
        if !self.wrote_header {
            try!(self.write_header());
            self.wrote_header = true;
        }

        try!(sample.write(&mut self.writer, self.spec.bits_per_sample));
        self.data_bytes_written += self.bytes_per_sample as u32;
        Ok(())
    }

    /// Performs finalization. After calling this, the writer should be destructed.
    fn finalize_internal(&mut self) -> Result<()> {
        self.finalized = true;

        // Flush remaining samples via the BufWriter.
        try!(self.writer.flush());

        // Extract the underlying writer and rewind it to the start, to update
        // the header fields of which we now know the value.
        let mut writer = self.writer.get_mut();

        // The header minus magic and 32-bit filesize is 60 bytes long.
        let file_size = self.data_bytes_written + 60;
        try!(writer.seek(io::SeekFrom::Start(4)));
        try!(writer.write_le_u32(file_size));
        try!(writer.seek(io::SeekFrom::Start(64)));
        try!(writer.write_le_u32(self.data_bytes_written));

        // Signal error if the last sample was not finished, but do so after
        // everything has been written, so that no data is lost, even though
        // the file is now ill-formed.
        if (self.data_bytes_written / self.bytes_per_sample as u32)
            % self.spec.channels as u32 != 0 {
            return Err(Error::UnfinishedSample);
        }

        Ok(())
    }

    /// Writes the parts of the WAVE format that require knowing all samples.
    ///
    /// This method must be called after all samples have been written. If it
    /// is not called, the destructor will finalize the file, but any errors
    /// that occur in the process cannot be observed in that manner.
    pub fn finalize(mut self) -> Result<()> {
        self.finalize_internal()
    }
}

impl<W> Drop for WavWriter<W> where W: io::Write + io::Seek {
    fn drop(&mut self) {
        // `finalize_internal` must be called only once. If that is done via
        // `finalize`, then this method is a no-op. If the user did not
        // finalize explicitly, then we should do it now. This can fail, but
        // drop should not panic, so a failure is ignored silently here.
        if !self.finalized {
            let _r = self.finalize_internal();
        }
    }
}

impl WavWriter<io::BufWriter<fs::File>> {
    /// Creates a writer that writes the WAVE format to a file.
    ///
    /// This is a convenience constructor that creates the file, wraps it in a
    /// `BufWriter`, and then constructs a `WavWriter` from it. The file will
    /// be overwritten if it exists.
    pub fn create<P: AsRef<path::Path>>(filename: P, spec: WavSpec)
           -> io::Result<WavWriter<io::BufWriter<fs::File>>> {
        let file = try!(fs::File::create(filename));
        let buf_writer = io::BufWriter::new(file);
        Ok(WavWriter::new(buf_writer, spec))
    }
}

#[test]
fn short_write_should_signal_error() {
    let mut buffer = io::Cursor::new(Vec::new());

    let write_spec = WavSpec {
        channels: 17,
        sample_rate: 48000,
        bits_per_sample: 8
    };

    // Deliberately write one sample less than 17 * 5.
    let mut writer = WavWriter::new(&mut buffer, write_spec);
    for s in (0 .. 17 * 5 - 1) {
        writer.write_sample(s as i16).unwrap();
    }
    let error = writer.finalize().err().unwrap();

    match error {
        Error::UnfinishedSample => { },
        _ => panic!("UnfinishedSample error should have been returned.")
    }
}

#[test]
fn wide_write_should_signal_error() {
    let mut buffer = io::Cursor::new(Vec::new());

    let spec8 = WavSpec {
        channels: 1,
        sample_rate: 44100,
        bits_per_sample: 8
    };
    {
        let mut writer = WavWriter::new(&mut buffer, spec8);
        assert!(writer.write_sample(127_i8).is_ok());
        assert!(writer.write_sample(127_i16).is_ok());
        assert!(writer.write_sample(127_i32).is_ok());
        assert!(writer.write_sample(128_i16).is_err());
        assert!(writer.write_sample(128_i32).is_err());
    }

    let spec16 = WavSpec { bits_per_sample: 16, .. spec8 };
    {
        let mut writer = WavWriter::new(&mut buffer, spec16);
        assert!(writer.write_sample(32767_i16).is_ok());
        assert!(writer.write_sample(32767_i32).is_ok());
        assert!(writer.write_sample(32768_i32).is_err());
    }

    let spec24 = WavSpec { bits_per_sample: 24, .. spec8 };
    {
        let mut writer = WavWriter::new(&mut buffer, spec24);
        assert!(writer.write_sample(8_388_607_i32).is_ok());
        assert!(writer.write_sample(8_388_608_i32).is_err());
    }
}
