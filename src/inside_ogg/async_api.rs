// Vorbis decoder written in Rust
//
// Copyright (c) 2016 est31 <MTest31@outlook.com>
// and contributors. All rights reserved.
// Licensed under MIT license, or Apache 2 license,
// at your option. Please see the LICENSE file
// attached to this source distribution for details.

/*!
Support for async I/O

This module provides support for asyncronous I/O.
*/

use crate::header::*;
use crate::VorbisError;
use std::io::{Read, Seek};
use crate::audio::{PreviousWindowRight, read_audio_packet,
    read_audio_packet_generic};
use crate::header::HeaderSet;
use crate::samples::{Samples, InterleavedSamples};
use ogg::Packet;
use ogg::OggReadError;
use ogg::reading::async_api::PacketReader;
use futures::stream::Stream;
use futures::{StreamExt, Future};
use tokio::io::AsyncRead;
use std::io::{Error, ErrorKind};
use std::mem::replace;
use std::pin::Pin;
use std::task::{Poll, Context};

pub async fn read_headers<T: AsyncRead + Unpin>(rdr: &mut PacketReader<T>) -> Result<HeaderSet, VorbisError> {
    macro_rules! rd_pck {
        () => {
            match rdr.next().await.transpose()? {
                Some(pck) => pck,
                None => {
                    Err(OggReadError::from(Error::new(ErrorKind::UnexpectedEof,
                        "Expected ogg packet but found end of physical stream")))?
                },
            }
        }
    }

    let ident = read_header_ident(&rd_pck!().data)?;
    let comment = read_header_comment(&rd_pck!().data)?;
    let setup = read_header_setup(&rd_pck!().data,
        ident.audio_channels, (ident.blocksize_0, ident.blocksize_1))?;

    Ok((ident, comment, setup))
}

/// Async ready creator utility to read headers out of an
/// ogg stream.
///
/// All functions this struct has are ready to be used for operation with async I/O.
pub struct HeadersReader<T: AsyncRead + Unpin> {
    pck_rd :PacketReader<T>,
    ident_hdr :Option<IdentHeader>,
    comment_hdr :Option<CommentHeader>,
}
impl<T: AsyncRead + Unpin> HeadersReader<T> {
    pub fn new(inner :T) -> Self {
        HeadersReader::from_packet_reader(PacketReader::new(inner))
    }
    pub fn from_packet_reader(pck_rd :PacketReader<T>) -> Self {
        HeadersReader {
            pck_rd,
            ident_hdr : None,
            comment_hdr : None,
        }
    }
}
impl<T: AsyncRead + Unpin> Future for HeadersReader<T> {
    type Output = Result<HeaderSet, VorbisError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        macro_rules! rd_pck {
            () => {
                if let Some(pck) = ready!(Pin::new(&mut self.pck_rd).poll_next(cx)?) {
                    pck
                } else {
                    // Note: we are stealing the Io variant from
                    // the ogg crate here which is not 100% clean,
                    // but I think in general it is what the
                    // read_packet_expected function of the ogg
                    // crate does too, and adding our own case
                    // to the VorbisError enum that only fires
                    // in an async mode is too complicated IMO.
                    Err(OggReadError::ReadError(Error::new(ErrorKind::UnexpectedEof,
                        "Expected header packet but found end of stream")))?
                }
            }
        }
        if self.ident_hdr.is_none() {
            let pck = rd_pck!();
            self.ident_hdr = Some(read_header_ident(&pck.data)?);
        }
        if self.comment_hdr.is_none() {
            let pck = rd_pck!();
            self.comment_hdr = Some(read_header_comment(&pck.data)?);
        }
        let setup_hdr = {
            let pck = rd_pck!();
            let ident = self.ident_hdr.as_ref().unwrap();
            read_header_setup(&pck.data,
                ident.audio_channels, (ident.blocksize_0, ident.blocksize_1))?
        };
        let ident_hdr = replace(&mut self.ident_hdr, None).unwrap();
        let comment_hdr = replace(&mut self.comment_hdr, None).unwrap();
        Poll::Ready(Ok((ident_hdr, comment_hdr, setup_hdr)))
    }
}
/// Reading ogg/vorbis files or streams
///
/// This is a small helper struct to help reading ogg/vorbis files
/// or streams in that format.
///
/// It only supports the main use case of pure audio ogg files streams.
/// Reading a file where vorbis is only one of multiple streams, like
/// in the case of ogv, is not supported.
///
/// If you need support for this, you need to use the lower level methods
/// instead.
pub struct OggStreamReader<T :AsyncRead + Unpin> {
    pck_rd :PacketReader<T>,
    pwr :PreviousWindowRight,

    pub ident_hdr :IdentHeader,
    pub comment_hdr :CommentHeader,
    pub setup_hdr :SetupHeader,

    absgp_of_last_read :Option<u64>,
}

impl<T :AsyncRead + Unpin> OggStreamReader<T> {
    /// Creates a new OggStreamReader from the given parameters
    pub fn new(hdr_rdr :HeadersReader<T>, hdrs :HeaderSet) -> Self {
        OggStreamReader::from_pck_rdr(hdr_rdr.pck_rd, hdrs)
    }
    /// Creates a new OggStreamReader from the given parameters
    pub fn from_pck_rdr(pck_rd :PacketReader<T>, hdrs :HeaderSet) -> Self {
        OggStreamReader {
            pck_rd,
            pwr : PreviousWindowRight::new(),

            ident_hdr : hdrs.0,
            comment_hdr : hdrs.1,
            setup_hdr : hdrs.2,

            absgp_of_last_read : None,
        }
    }
}

impl<T :AsyncRead + Unpin> Stream for OggStreamReader<T> {
    type Item = Result<Vec<Vec<i16>>, VorbisError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        let pck = match ready!(Pin::new(&mut this.pck_rd).poll_next(cx)?) {
            Some(p) => p,
            None => return Poll::Ready(None),
        };
        let decoded_pck = read_audio_packet(&this.ident_hdr,
            &this.setup_hdr, &pck.data, &mut this.pwr)?;
        self.absgp_of_last_read = Some(pck.absgp_page());
        Poll::Ready(Some(Ok(decoded_pck)))
    }
}
