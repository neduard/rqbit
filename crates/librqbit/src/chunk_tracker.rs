use librqbit_core::lengths::{ChunkInfo, Lengths, ValidPieceIndex};
use log::{debug, info};
use peer_binary_protocol::Piece;

use crate::type_aliases::BF;

pub struct ChunkTracker {
    // This forms the basis of a "queue" to pull from.
    // It's set to 1 if we need a piece, but the moment we start requesting a peer,
    // it's set to 0.

    // Better to rename into piece_queue or smth, and maybe use some other form of a queue.
    needed_pieces: BF,

    // This has a bit set per each chunk (block) that we have written to the output file.
    // It doesn't mean it's valid yet. Used to track how much is left in each piece.
    chunk_status: BF,

    // These are the pieces that we actually have, fully checked and downloaded.
    have: BF,

    lengths: Lengths,

    priority_piece_ids: Vec<usize>,
}

// TODO: this should be redone from "have" pieces, not from "needed" pieces.
// Needed pieces are the ones we need to download, not necessarily the ones we have.
// E.g. we might have more pieces, but the client asks to download only some files
// partially.
fn compute_chunk_status(lengths: &Lengths, needed_pieces: &BF) -> BF {
    let required_size = lengths.chunk_bitfield_bytes();
    let vec = vec![0u8; required_size];
    let mut chunk_bf = BF::from_vec(vec);
    for piece_index in needed_pieces
        .get(0..lengths.total_pieces() as usize)
        .unwrap()
        .iter_zeros()
    {
        let offset = piece_index * lengths.default_chunks_per_piece() as usize;
        let chunks_per_piece = lengths
            .chunks_per_piece(lengths.validate_piece_index(piece_index as u32).unwrap())
            as usize;
        chunk_bf
            .get_mut(offset..offset + chunks_per_piece)
            .unwrap()
            .fill(true);
    }
    chunk_bf
}

pub enum ChunkMarkingResult {
    PreviouslyCompleted,
    NotCompleted,
    Completed,
}

impl ChunkTracker {
    pub fn new(needed_pieces: BF, have_pieces: BF, lengths: Lengths) -> Self {
        // TODO: ideally this needs to be a list based on needed files, e.g.
        // last needed piece for each file. But let's keep simple for now.
        let last_needed_piece_id = needed_pieces.iter_ones().next_back();

        // The last pieces first. Often important information is stored in the last piece.
        // E.g. if it's a video file, than the last piece often contains some index, or just
        // players look into it, and it's better be there.
        let priority_piece_ids = last_needed_piece_id.into_iter().collect();
        Self {
            chunk_status: compute_chunk_status(&lengths, &needed_pieces),
            needed_pieces,
            lengths,
            have: have_pieces,
            priority_piece_ids,
        }
    }
    pub fn get_needed_pieces(&self) -> &BF {
        &self.needed_pieces
    }
    pub fn get_have_pieces(&self) -> &BF {
        &self.have
    }
    pub fn reserve_needed_piece(&mut self, index: ValidPieceIndex) {
        self.needed_pieces.set(index.get() as usize, false)
    }

    pub fn iter_needed_pieces(&self) -> impl Iterator<Item = usize> + '_ {
        self.priority_piece_ids
            .iter()
            .copied()
            .filter(move |piece_id| self.needed_pieces[*piece_id])
            .chain(
                self.needed_pieces
                    .iter_ones()
                    .filter(move |id| !self.priority_piece_ids.contains(id)),
            )
    }

    // None if wrong chunk
    // true if did something
    // false if didn't do anything
    pub fn mark_chunk_request_cancelled(
        &mut self,
        index: ValidPieceIndex,
        _chunk: u32,
    ) -> Option<bool> {
        if *self.have.get(index.get() as usize)? {
            return Some(false);
        }
        // This will trigger the requesters to re-check each chunk in this piece.
        let chunk_range = self.lengths.chunk_range(index);
        if !self.chunk_status.get(chunk_range)?.all() {
            self.needed_pieces.set(index.get() as usize, true);
        }
        Some(true)
    }

    pub fn mark_piece_broken(&mut self, index: ValidPieceIndex) -> bool {
        info!("remarking piece={} as broken", index);
        self.needed_pieces.set(index.get() as usize, true);
        self.chunk_status
            .get_mut(self.lengths.chunk_range(index))
            .map(|s| {
                s.fill(false);
                true
            })
            .unwrap_or_default()
    }

    pub fn mark_piece_downloaded(&mut self, idx: ValidPieceIndex) {
        self.have.set(idx.get() as usize, true);
    }

    pub fn is_chunk_downloaded(&self, chunk: &ChunkInfo) -> bool {
        *self
            .chunk_status
            .get(chunk.absolute_index as usize)
            .unwrap()
    }

    pub fn is_chunk_ready_to_upload(&self, chunk: &ChunkInfo) -> bool {
        self.have
            .get(chunk.piece_index.get() as usize)
            .map(|b| *b)
            .unwrap_or(false)
    }

    // return true if the whole piece is marked downloaded
    pub fn mark_chunk_downloaded<ByteBuf>(
        &mut self,
        piece: &Piece<ByteBuf>,
    ) -> Option<ChunkMarkingResult>
    where
        ByteBuf: AsRef<[u8]>,
    {
        let chunk_info = self.lengths.chunk_info_from_received_piece(
            piece.index,
            piece.begin,
            piece.block.as_ref().len() as u32,
        )?;
        let chunk_range = self.lengths.chunk_range(chunk_info.piece_index);
        let chunk_range = self.chunk_status.get_mut(chunk_range).unwrap();
        if chunk_range.all() {
            return Some(ChunkMarkingResult::PreviouslyCompleted);
        }
        chunk_range.set(chunk_info.chunk_index as usize, true);
        debug!(
            "piece={}, chunk_info={:?}, bits={:?}",
            piece.index, chunk_info, chunk_range,
        );

        // TODO: remove me, it's for debugging
        // {
        //     use std::io::Write;
        //     let mut f = std::fs::OpenOptions::new()
        //         .write(true)
        //         .create(true)
        //         .open("/tmp/chunks")
        //         .unwrap();
        //     write!(f, "{:?}", &self.have).unwrap();
        // }

        if chunk_range.all() {
            return Some(ChunkMarkingResult::Completed);
        }
        Some(ChunkMarkingResult::NotCompleted)
    }
}
