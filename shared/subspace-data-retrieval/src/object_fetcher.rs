// Copyright (C) 2024 Subspace Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Fetching objects stored in the archived history of Subspace Network.

use async_trait::async_trait;
use parity_scale_codec::{Compact, CompactLen, Decode, Encode};
use std::fmt;
use std::sync::Arc;
use subspace_archiving::archiver::{NewArchivedSegment, Segment, SegmentItem};
use subspace_core_primitives::{
    Piece, PieceIndex, RawRecord, RecordedHistorySegment, SegmentIndex,
};
use tracing::{debug, trace};

/// Object fetching errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Supplied piece index is not a source piece
    #[error("Piece index {piece_index} is not a source piece, offset: {piece_offset}")]
    NotSourcePiece {
        piece_index: PieceIndex,
        piece_offset: u32,
    },

    /// No item in segment at offset
    #[error("Offset {offset_in_segment} in segment {segment_index} is not an item, current progress: {progress}, object: {piece_index:?}, {piece_offset}")]
    NoSegmentItem {
        progress: usize,
        offset_in_segment: usize,
        segment_index: SegmentIndex,
        piece_index: PieceIndex,
        piece_offset: u32,
    },

    /// Unexpected item in first segment at offset
    #[error("Offset {offset_in_segment} in first segment {segment_index} has unexpected item, current progress: {segment_progress}, object: {piece_index:?}, {piece_offset}, item: {segment_item:?}")]
    UnexpectedFirstSegmentItem {
        segment_progress: usize,
        offset_in_segment: usize,
        segment_index: SegmentIndex,
        segment_item: Box<SegmentItem>,
        piece_index: PieceIndex,
        piece_offset: u32,
    },

    /// Unexpected item in continuing segment at offset
    #[error("Continuing segment {segment_index} has unexpected item, collected data: {collected_data}, object: {piece_index:?}, {piece_offset}, item: {segment_item:?}")]
    UnexpectedContinuingSegmentItem {
        collected_data: usize,
        segment_index: SegmentIndex,
        segment_item: Box<SegmentItem>,
        piece_index: PieceIndex,
        piece_offset: u32,
    },

    /// Object not found after downloading expected number of segments
    #[error("Object segment range {first_segment_index}..={last_segment_index} did not contain full object, object: {piece_index:?}, {piece_offset}")]
    TooManySegments {
        first_segment_index: SegmentIndex,
        last_segment_index: SegmentIndex,
        piece_index: PieceIndex,
        piece_offset: u32,
    },

    /// Object is too large error
    #[error(
        "Data length {data_length} exceeds maximum object size {max_object_len} for object: {piece_index:?}, {piece_offset}"
    )]
    ObjectTooLarge {
        data_length: usize,
        max_object_len: usize,
        piece_index: PieceIndex,
        piece_offset: u32,
    },

    /// Length prefix is too large error
    #[error(
        "Length prefix length {length_prefix_len} exceeds maximum object size {max_object_len} for object: {piece_index:?}, {piece_offset}"
    )]
    LengthPrefixTooLarge {
        length_prefix_len: usize,
        max_object_len: usize,
        piece_index: PieceIndex,
        piece_offset: u32,
    },

    /// Object decoding error
    #[error("Object data decoding error: {source:?}")]
    ObjectDecoding {
        #[from]
        source: parity_scale_codec::Error,
    },

    /// Segment getter error
    #[error("Getting segment failed: {source:?}")]
    SegmentGetter {
        #[from]
        source: SegmentGetterError,
    },

    /// Piece getter error
    #[error("Getting piece failed temporarily: {source:?}")]
    PieceGetterTemporary {
        #[from]
        source: PieceGetterError,
    },

    /// Piece getter custom error type
    #[error("Getting piece failed permanently: {source:?}")]
    PieceGetterPermanent { source: BoxError },
}

/// Segment getter errors.
#[derive(Debug, thiserror::Error)]
pub enum SegmentGetterError {
    /// Segment not found
    #[error("Segment index {segment_index} is not available")]
    NotFound { segment_index: PieceIndex },

    /// Segment decoding error
    #[error("Segment data decoding error: {source:?}")]
    SegmentDecoding {
        #[from]
        source: parity_scale_codec::Error,
    },

    /// Piece getter error
    #[error("Getting piece failed: {source:?}")]
    PieceGetter {
        #[from]
        source: PieceGetterError,
    },
}

/// Piece getter errors.
#[derive(Debug, thiserror::Error)]
pub enum PieceGetterError {
    /// Piece not found
    #[error("Piece index {piece_index} is not available from this provider")]
    NotFound { piece_index: PieceIndex },

    /// Piece decoding error
    #[error("Piece data decoding error: {source:?}")]
    PieceDecoding {
        #[from]
        source: parity_scale_codec::Error,
    },
}

/// A type-erased error
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Something that can be used to get decoded pieces by index
#[async_trait]
pub trait ObjectPieceGetter: fmt::Debug {
    /// Get piece by index.
    ///
    /// Returns `Ok(None)` for temporary errors: the piece is not found, but immediately retrying
    /// this provider might return it.
    /// Returns `Err(_)` for permanent errors: this provider can't provide the piece at this time,
    /// and another provider should be attempted.
    async fn get_piece(&self, piece_index: PieceIndex) -> Result<Option<Piece>, BoxError>;
}

#[async_trait]
impl<PG> ObjectPieceGetter for Arc<PG>
where
    PG: ObjectPieceGetter + Send + Sync + ?Sized,
{
    async fn get_piece(&self, piece_index: PieceIndex) -> Result<Option<Piece>, BoxError> {
        self.as_ref().get_piece(piece_index).await
    }
}

// Convenience methods, mainly used in testing
#[async_trait]
impl ObjectPieceGetter for NewArchivedSegment {
    async fn get_piece(&self, piece_index: PieceIndex) -> Result<Option<Piece>, BoxError> {
        if piece_index.segment_index() == self.segment_header.segment_index() {
            return Ok(Some(
                self.pieces
                    .pieces()
                    .nth(piece_index.position() as usize)
                    .expect("checked segment index in if; piece must be present; qed"),
            ));
        }

        Err(PieceGetterError::NotFound { piece_index }.into())
    }
}

#[async_trait]
impl ObjectPieceGetter for (PieceIndex, Piece) {
    async fn get_piece(&self, piece_index: PieceIndex) -> Result<Option<Piece>, BoxError> {
        if self.0 == piece_index {
            return Ok(Some(self.1.clone()));
        }

        Err(PieceGetterError::NotFound { piece_index }.into())
    }
}

// TODO: impl for IntoIterator instead?
#[async_trait]
impl ObjectPieceGetter for Vec<(PieceIndex, Piece)> {
    async fn get_piece(&self, piece_index: PieceIndex) -> Result<Option<Piece>, BoxError> {
        for (index, piece) in self.iter() {
            if *index == piece_index {
                return Ok(Some(piece.clone()));
            }
        }

        Err(PieceGetterError::NotFound { piece_index }.into())
    }
}

/// Object fetcher for the Subspace DSN.
pub struct ObjectFetcher {
    /// The piece getter used to fetch pieces.
    piece_getter: Arc<dyn ObjectPieceGetter + Send + Sync + 'static>,

    /// The maximum number of data bytes we'll read for a single object.
    max_object_len: usize,
}

impl ObjectFetcher {
    /// Create a new object fetcher with the given piece getter.
    ///
    /// `max_object_len` is the amount of data bytes we'll read for a single object before giving
    /// up and returning an error, or `None` for no limit (`usize::MAX`).
    pub fn new<PG>(piece_getter: PG, max_object_len: Option<usize>) -> Self
    where
        PG: ObjectPieceGetter + Send + Sync + 'static,
    {
        Self {
            piece_getter: Arc::new(piece_getter),
            max_object_len: max_object_len.unwrap_or(usize::MAX),
        }
    }

    /// Assemble the object in `piece_index` at `piece_offset` by fetching necessary pieces using
    /// the piece getter and putting the object's bytes together.
    ///
    /// The caller should check the object's hash to make sure the correct bytes are returned.
    pub async fn fetch_object(
        &self,
        piece_index: PieceIndex,
        piece_offset: u32,
    ) -> Result<Vec<u8>, Error> {
        if !piece_index.is_source() {
            tracing::debug!(
                %piece_index,
                piece_offset,
                "Invalid piece index for object: must be a source piece",
            );

            // Parity pieces contain effectively random data, and can't be used to fetch objects
            return Err(Error::NotSourcePiece {
                piece_index,
                piece_offset,
            });
        }

        // Try fast object assembling from individual pieces
        if let Some(data) = self.fetch_object_fast(piece_index, piece_offset).await? {
            tracing::debug!(
                %piece_index,
                piece_offset,
                len = %data.len(),
                "Fetched object using fast object assembling",
            );

            return Ok(data);
        }

        // Regular object assembling from segments
        let data = self.fetch_object_regular(piece_index, piece_offset).await?;

        tracing::debug!(
            %piece_index,
            piece_offset,
            len = %data.len(),
            "Fetched object using regular object assembling",
        );

        Ok(data)
    }

    /// Fast object fetching and assembling where the object doesn't cross piece (super fast) or
    /// segment (just fast) boundaries, returns `Ok(None)` if fast retrieval is not guaranteed.
    // TODO: return already downloaded pieces from fetch_object_fast() and pass them to fetch_object_regular()
    async fn fetch_object_fast(
        &self,
        piece_index: PieceIndex,
        piece_offset: u32,
    ) -> Result<Option<Vec<u8>>, Error> {
        // If the offset is before the last 2 bytes of a segment, we might be able to do very fast
        // object retrieval without assembling and processing the whole segment.
        //
        // The last 2 bytes might contain padding if a piece is the last piece in the segment.
        let before_last_two_bytes = piece_offset as usize <= RawRecord::SIZE - 1 - 2;
        let piece_position_in_segment = piece_index.position();
        let data_shards = RecordedHistorySegment::NUM_RAW_RECORDS as u32;
        let last_data_piece_in_segment = piece_position_in_segment >= data_shards - 1;

        if last_data_piece_in_segment && !before_last_two_bytes {
            // Fast retrieval possibility is not guaranteed
            return Ok(None);
        }

        // How much bytes are definitely available starting at `piece_index` and `offset` without
        // crossing a segment boundary.
        //
        // The last 2 bytes might contain padding if a piece is the last piece in the segment.
        let bytes_available_in_segment =
            (data_shards - piece_position_in_segment) * RawRecord::SIZE as u32 - piece_offset - 2;

        // Data from pieces that were already read, starting with piece at index `piece_index`
        let mut read_records_data = Vec::<u8>::with_capacity(RawRecord::SIZE * 2);
        let mut next_source_piece_index = piece_index;

        let piece = self
            .read_piece(next_source_piece_index, piece_index, piece_offset)
            .await?;
        next_source_piece_index = next_source_piece_index.next_source_index();
        read_records_data.extend(piece.record().to_raw_record_chunks().flatten().copied());

        if last_data_piece_in_segment {
            // The last 2 bytes might contain segment padding, so we can't use them for object length or object data.
            read_records_data.truncate(RawRecord::SIZE - 2);
        }

        let data_length = self.decode_data_length(
            &read_records_data[piece_offset as usize..],
            piece_index,
            piece_offset,
        )?;

        let data_length = if let Some(data_length) = data_length {
            data_length
        } else if !last_data_piece_in_segment {
            // Need the next piece to read the length of data, but we can only use it if there was
            // no segment padding
            let piece = self
                .read_piece(next_source_piece_index, piece_index, piece_offset)
                .await?;
            next_source_piece_index = next_source_piece_index.next_source_index();
            read_records_data.extend(piece.record().to_raw_record_chunks().flatten().copied());

            self.decode_data_length(
                &read_records_data[piece_offset as usize..],
                piece_index,
                piece_offset,
            )?
            .expect("Extra RawRecord is larger than the length encoding; qed")
        } else {
            // Super fast read is not possible, because we removed potential segment padding, so
            // the piece bytes are not guaranteed to be continuous
            return Ok(None);
        };

        if data_length > bytes_available_in_segment as usize {
            // Not enough data without crossing segment boundary
            return Ok(None);
        }

        // Discard piece data before the offset
        let mut data = read_records_data[piece_offset as usize..].to_vec();
        drop(read_records_data);

        // Read more pieces until we have enough data
        let remaining_piece_count = (data_length as usize - data.len()) / RawRecord::SIZE;
        let remaining_piece_indexes = (next_source_piece_index..)
            .filter(|i| i.is_source())
            .take(remaining_piece_count);
        self.read_pieces(remaining_piece_indexes, piece_index, piece_offset)
            .await?
            .into_iter()
            .for_each(|piece| {
                data.extend(piece.record().to_raw_record_chunks().flatten().copied())
            });

        // Decode the data, and return it if it's valid
        let data = Vec::<u8>::decode(&mut data.as_slice())?;

        Ok(Some(data))
    }

    /// Fetch and assemble an object that can cross segment boundaries, which requires assembling
    /// and iterating over full segments.
    async fn fetch_object_regular(
        &self,
        piece_index: PieceIndex,
        piece_offset: u32,
    ) -> Result<Vec<u8>, Error> {
        let segment_index = piece_index.segment_index();
        let piece_position_in_segment = piece_index.position();
        // Used to access the data after it is converted to raw bytes
        let offset_in_segment =
            piece_position_in_segment as usize * RawRecord::SIZE + piece_offset as usize;

        let mut data = {
            let Segment::V0 { items } = self
                .read_segment(segment_index, piece_index, piece_offset)
                .await?;
            // Go through the segment until we reach the offset.
            // Unconditional progress is enum variant + compact encoding of number of elements
            let mut progress = 1 + Compact::compact_len(&(items.len() as u64));
            let segment_item = items
                .into_iter()
                .find(|item| {
                    // Add number of bytes in encoded version of segment item
                    progress += item.encoded_size();

                    // Our data is within another segment item, which will have wrapping data
                    // structure, hence strictly `>` here
                    progress > offset_in_segment
                })
                .ok_or_else(|| {
                    debug!(
                        progress,
                        offset_in_segment,
                        ?segment_index,
                        %piece_index,
                        piece_offset,
                        "Failed to find item at offset in segment"
                    );

                    Error::NoSegmentItem {
                        progress,
                        offset_in_segment,
                        segment_index,
                        piece_index,
                        piece_offset,
                    }
                })?;

            // Look at the item after the offset, collecting block bytes
            match segment_item {
                SegmentItem::Block { bytes, .. }
                | SegmentItem::BlockStart { bytes, .. }
                | SegmentItem::BlockContinuation { bytes, .. } => {
                    // Rewind back progress to the beginning of the number of bytes
                    progress -= bytes.len();
                    // Get a chunk of the bytes starting at the position we care about
                    Vec::from(&bytes[offset_in_segment - progress..])
                }
                segment_item @ SegmentItem::Padding
                | segment_item @ SegmentItem::ParentSegmentHeader(_) => {
                    // TODO: create a Display impl for SegmentItem that is shorter than the entire
                    // data contained in it
                    debug!(
                        segment_progress = progress,
                        offset_in_segment,
                        %segment_index,
                        %piece_index,
                        piece_offset,
                        ?segment_item,
                        "Unexpected segment item in first segment",
                    );

                    return Err(Error::UnexpectedFirstSegmentItem {
                        segment_progress: progress,
                        offset_in_segment,
                        segment_index,
                        piece_index,
                        piece_offset,
                        segment_item: Box::new(segment_item),
                    });
                }
            }
        };

        // Return an error if the length is unreasonably large, before we get the next segment
        if let Some(data_length) =
            self.decode_data_length(data.as_slice(), piece_index, piece_offset)?
        {
            // If we have the whole object, decode and return it.
            // TODO: use tokio Bytes type to re-use the same allocation by stripping the length at the start
            if data.len() >= data_length {
                return Ok(Vec::<u8>::decode(&mut data.as_slice())?);
            }
        }

        // We need to read extra segments to get the object length, or the full object.
        // We don't attempt to calculate the number of segments needed, because it involves
        // headers and optional padding.
        loop {
            let Segment::V0 { items } = self
                .read_segment(segment_index + SegmentIndex::ONE, piece_index, piece_offset)
                .await?;
            for segment_item in items {
                match segment_item {
                    SegmentItem::BlockContinuation { bytes, .. } => {
                        data.extend_from_slice(&bytes);

                        if let Some(data_length) =
                            self.decode_data_length(data.as_slice(), piece_index, piece_offset)?
                        {
                            if data.len() >= data_length {
                                return Ok(Vec::<u8>::decode(&mut data.as_slice())?);
                            }
                        }
                    }

                    // Padding at the end of segments can be skipped, it's not part of the object data
                    SegmentItem::Padding => {}

                    // We should not see these items while collecting data for a single object
                    SegmentItem::Block { .. }
                    | SegmentItem::BlockStart { .. }
                    | SegmentItem::ParentSegmentHeader(_) => {
                        debug!(
                            collected_data = ?data.len(),
                            %segment_index,
                            %piece_index,
                            piece_offset,
                            ?segment_item,
                            "Unexpected segment item in continuing segment",
                        );

                        return Err(Error::UnexpectedContinuingSegmentItem {
                            collected_data: data.len(),
                            segment_index,
                            piece_index,
                            piece_offset,
                            segment_item: Box::new(segment_item),
                        });
                    }
                }
            }
        }
    }

    /// Read the whole segment by its index (just records, skipping witnesses).
    ///
    /// The mapping piece index and offset are only used for error reporting.
    // TODO: replace with a refactored subspace-service::sync_from_dsn::import_blocks::download_and_reconstruct_blocks()
    async fn read_segment(
        &self,
        _segment_index: SegmentIndex,
        _mapping_piece_index: PieceIndex,
        _mapping_piece_offset: u32,
    ) -> Result<Segment, Error> {
        unimplemented!("read_segment will be implemented as part of a refactoring")
    }

    /// Concurrently read multiple pieces by their indexes
    ///
    /// The mapping piece index and offset are only used for error reporting.
    // TODO: replace with a refactored method that fetches pieces
    async fn read_pieces(
        &self,
        _piece_indexes: impl IntoIterator<Item = PieceIndex>,
        _mapping_piece_index: PieceIndex,
        _mapping_piece_offset: u32,
    ) -> Result<Vec<Piece>, Error> {
        unimplemented!("read_pieces will be implemented as part of a refactoring")
    }

    /// Read and return a single piece.
    ///
    /// The mapping piece index and offset are only used for error reporting.
    async fn read_piece(
        &self,
        piece_index: PieceIndex,
        mapping_piece_index: PieceIndex,
        mapping_piece_offset: u32,
    ) -> Result<Piece, Error> {
        let piece = self
            .piece_getter
            .get_piece(piece_index)
            .await
            .map_err(|source| {
                debug!(
                    %piece_index,
                    error = ?source,
                    %mapping_piece_index,
                    mapping_piece_offset,
                    "Permanent error fetching piece during object assembling"
                );

                Error::PieceGetterPermanent { source }
            })?;

        if let Some(piece) = piece {
            trace!(
                %piece_index,
                %mapping_piece_index,
                mapping_piece_offset,
                "Fetched piece during object assembling"
            );

            Ok(piece)
        } else {
            debug!(
                %piece_index,
                %mapping_piece_index,
                mapping_piece_offset,
                "Temporary error fetching piece during object assembling"
            );

            Err(PieceGetterError::NotFound {
                piece_index: mapping_piece_index,
            })?
        }
    }

    /// Validate and decode the encoded length of `data`, including the encoded length bytes.
    /// `data` may be incomplete.
    ///
    /// Returns `Ok(Some(data_length_encoded_length + data_length))` if the length is valid,
    /// `Ok(None)` if there aren't enough bytes to decode the length, otherwise an error.
    ///
    /// The mapping piece index and offset are only used for error reporting.
    fn decode_data_length(
        &self,
        mut data: &[u8],
        mapping_piece_index: PieceIndex,
        mapping_piece_offset: u32,
    ) -> Result<Option<usize>, Error> {
        let data_length = match Compact::<u64>::decode(&mut data) {
            Ok(Compact(data_length)) => {
                let data_length = data_length as usize;
                if data_length > self.max_object_len {
                    debug!(
                        data_length,
                        max_object_len = self.max_object_len,
                        %mapping_piece_index,
                        mapping_piece_offset,
                        "Data length exceeds object size limit for object fetcher"
                    );

                    return Err(Error::ObjectTooLarge {
                        data_length,
                        max_object_len: self.max_object_len,
                        piece_index: mapping_piece_index,
                        piece_offset: mapping_piece_offset,
                    });
                }

                data_length
            }
            Err(err) => {
                // Parity doesn't have an easily matched error enum, and all bit sequences are
                // valid compact encodings. So we assume we don't have enough bytes to decode the
                // length, unless we already have enough bytes to decode the maximum length.
                if data.len() >= Compact::<u64>::compact_len(&(self.max_object_len as u64)) {
                    debug!(
                        length_prefix_len = data.len(),
                        max_object_len = self.max_object_len,
                        %mapping_piece_index,
                        mapping_piece_offset,
                        "Length prefix exceeds object size limit for object fetcher"
                    );

                    return Err(Error::LengthPrefixTooLarge {
                        length_prefix_len: data.len(),
                        max_object_len: self.max_object_len,
                        piece_index: mapping_piece_index,
                        piece_offset: mapping_piece_offset,
                    });
                }

                debug!(
                    ?err,
                    %mapping_piece_index,
                    mapping_piece_offset,
                    "Not enough bytes to decode data length for object"
                );

                return Ok(None);
            }
        };

        let data_length_encoded_length = Compact::<u64>::compact_len(&(data_length as u64));

        trace!(
            data_length,
            data_length_encoded_length,
            %mapping_piece_index,
            mapping_piece_offset,
            "Decoded data length for object"
        );

        Ok(Some(data_length_encoded_length + data_length))
    }
}
