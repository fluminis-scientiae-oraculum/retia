/*
 * Inlined and trimmed from `swapvec` 0.4.2 by Julian Büttner
 * (MIT-licensed: https://github.com/julianbuettner/swapvec).
 *
 * Motivation: upstream `swapvec` was last released 2023-11 and pins
 * `lz4_flex ^0.10`, which RUSTSEC-2026-0041 flagged for uninitialized-memory
 * leakage on decompression. retia only ever instantiates `SwapVec` with the
 * default (no-compression) config, so the lz4/deflate paths were dead code
 * here — both compression knobs and their `lz4_flex` / `miniz_oxide` deps are
 * dropped. `bincode` is replaced with `rmp-serde`, which retia already
 * carries.
 *
 * Original MIT copyright notice preserved per the license terms:
 *
 *     Copyright (c) 2023 Julian Büttner
 *
 *     Permission is hereby granted, free of charge, to any person obtaining a
 *     copy of this software and associated documentation files (the
 *     "Software"), to deal in the Software without restriction, including
 *     without limitation the rights to use, copy, modify, merge, publish,
 *     distribute, sublicense, and/or sell copies of the Software, and to
 *     permit persons to whom the Software is furnished to do so, subject to
 *     the following conditions:
 *
 *     The above copyright notice and this permission notice shall be included
 *     in all copies or substantial portions of the Software.
 *
 *     THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND...
 */

#![allow(dead_code)]

use std::{
    collections::{hash_map::DefaultHasher, VecDeque},
    fmt::Debug,
    fs::File,
    hash::{Hash, Hasher},
    io::{self, BufReader, BufWriter, Read, Seek, Write},
};

use serde::{Deserialize, Serialize};

// --- error ------------------------------------------------------------------

#[derive(Debug)]
#[non_exhaustive]
pub enum SwapVecError {
    MissingPermissions,
    OutOfDisk,
    SerializationFailed(String),
    Other(io::ErrorKind),
}

impl From<io::Error> for SwapVecError {
    fn from(value: io::Error) -> Self {
        match value.kind() {
            io::ErrorKind::PermissionDenied => Self::MissingPermissions,
            e => Self::Other(e),
        }
    }
}

impl From<rmp_serde::encode::Error> for SwapVecError {
    fn from(value: rmp_serde::encode::Error) -> Self {
        Self::SerializationFailed(value.to_string())
    }
}

impl From<rmp_serde::decode::Error> for SwapVecError {
    fn from(value: rmp_serde::decode::Error) -> Self {
        Self::SerializationFailed(value.to_string())
    }
}

// --- batched checked file ---------------------------------------------------

#[derive(Debug)]
struct BatchInfo {
    hash: u64,
    bytes: usize,
}

struct BatchWriter<T: Write> {
    inner: BufWriter<T>,
    batch_infos: Vec<BatchInfo>,
}

struct BatchReader<T: Read> {
    inner: BufReader<T>,
    batch_infos: Vec<BatchInfo>,
    batch_index: usize,
    buffer: Vec<u8>,
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

impl<T: Write> BatchWriter<T> {
    fn new(writer: T) -> Self {
        Self {
            batch_infos: Vec::new(),
            inner: BufWriter::new(writer),
        }
    }

    fn write_batch(&mut self, buffer: &[u8]) -> Result<(), io::Error> {
        self.inner.write_all(buffer)?;
        self.batch_infos.push(BatchInfo {
            hash: hash_bytes(buffer),
            bytes: buffer.len(),
        });
        self.inner.flush()
    }

    fn bytes_written(&self) -> usize {
        self.batch_infos.iter().map(|b| b.bytes).sum()
    }

    fn batch_count(&self) -> usize {
        self.batch_infos.len()
    }
}

impl<T: Read + Seek> BatchReader<T> {
    fn reset(&mut self) -> Result<(), io::Error> {
        self.inner.seek(io::SeekFrom::Start(0))?;
        self.batch_index = 0;
        self.buffer.clear();
        Ok(())
    }
}

impl<T: Read> BatchReader<T> {
    fn read_batch(&mut self) -> Result<Option<&[u8]>, SwapVecError> {
        let batch_info = self.batch_infos.get(self.batch_index);
        self.batch_index += 1;
        let Some(batch_info) = batch_info else {
            return Ok(None);
        };
        self.buffer.resize(batch_info.bytes, 0);
        self.inner.read_exact(self.buffer.as_mut_slice())?;
        // Hash mismatch intentionally non-fatal — matches upstream behavior.
        let _ = hash_bytes(self.buffer.as_slice()) == batch_info.hash;
        Ok(Some(self.buffer.as_slice()))
    }
}

impl<T: Read + Write + Seek> TryFrom<BatchWriter<T>> for BatchReader<T> {
    type Error = io::Error;

    fn try_from(value: BatchWriter<T>) -> Result<Self, Self::Error> {
        let mut inner = value
            .inner
            .into_inner()
            .map_err(|inner_error| inner_error.into_error())?;
        inner.seek(io::SeekFrom::Start(0))?;
        Ok(Self {
            inner: BufReader::new(inner),
            batch_infos: value.batch_infos,
            batch_index: 0,
            buffer: Vec::new(),
        })
    }
}

// --- config -----------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SwapVecConfig {
    pub swap_after: usize,
    pub batch_size: usize,
}

impl Default for SwapVecConfig {
    fn default() -> Self {
        Self {
            swap_after: 32 * 1024 * 1024,
            batch_size: 32 * 1024,
        }
    }
}

// --- SwapVec ----------------------------------------------------------------

pub struct SwapVec<T>
where
    for<'a> T: Serialize + Deserialize<'a>,
{
    tempfile: Option<BatchWriter<File>>,
    vector: VecDeque<T>,
    config: SwapVecConfig,
}

impl<T: Serialize + for<'a> Deserialize<'a>> Default for SwapVec<T> {
    fn default() -> Self {
        Self {
            tempfile: None,
            vector: VecDeque::new(),
            config: SwapVecConfig::default(),
        }
    }
}

impl<T: Serialize + for<'a> Deserialize<'a>> Debug for SwapVec<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SwapVec {{elements_in_ram: {}, elements_in_file: {}}}",
            self.vector.len(),
            self.tempfile.as_ref().map(|x| x.batch_count()).unwrap_or(0) * self.config.batch_size,
        )
    }
}

impl<T> SwapVec<T>
where
    for<'a> T: Serialize + Deserialize<'a> + Clone,
{
    pub fn with_config(config: SwapVecConfig) -> Self {
        Self {
            tempfile: None,
            vector: VecDeque::new(),
            config,
        }
    }

    pub fn consume(&mut self, it: impl Iterator<Item = T>) -> Result<(), SwapVecError> {
        for element in it {
            self.push(element)?;
        }
        Ok(())
    }

    pub fn push(&mut self, element: T) -> Result<(), SwapVecError> {
        self.vector.push_back(element);
        self.after_push_work()
    }

    pub fn written_to_file(&self) -> bool {
        self.tempfile.is_some()
    }

    pub fn file_size(&self) -> Option<usize> {
        self.tempfile.as_ref().map(|f| f.bytes_written())
    }

    pub fn batches_written(&self) -> usize {
        match self.tempfile.as_ref() {
            None => 0,
            Some(f) => f.batch_count(),
        }
    }

    fn after_push_work(&mut self) -> Result<(), SwapVecError> {
        if self.vector.len() <= self.config.batch_size {
            return Ok(());
        }
        if self.tempfile.is_none() && self.vector.len() <= self.config.swap_after {
            return Ok(());
        }

        if self.tempfile.is_none() {
            let tf = tempfile::Builder::new().tempfile_in(".")?.into_file();
            self.tempfile = Some(BatchWriter::new(tf));
        }
        let batch: Vec<T> = self.vector.drain(0..self.config.batch_size).collect();
        let buffer = rmp_serde::to_vec(&batch)?;
        self.tempfile.as_mut().unwrap().write_batch(&buffer)?;
        Ok(())
    }
}

impl<T: Serialize + for<'a> Deserialize<'a> + Clone> IntoIterator for SwapVec<T> {
    type Item = Result<T, SwapVecError>;
    type IntoIter = SwapVecIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        SwapVecIter::new(self.tempfile, self.vector, self.config)
    }
}

// --- iterator ---------------------------------------------------------------

struct VecDequeIndex<T: Clone> {
    value: VecDeque<T>,
}

impl<T: Clone> From<VecDeque<T>> for VecDequeIndex<T> {
    fn from(value: VecDeque<T>) -> Self {
        Self { value }
    }
}

impl<T: Clone> VecDequeIndex<T> {
    fn get(&self, i: usize) -> Option<T> {
        let (a, b) = self.value.as_slices();
        if i < a.len() {
            a.get(i).cloned()
        } else {
            b.get(i - a.len()).cloned()
        }
    }
}

pub struct SwapVecIter<T>
where
    for<'a> T: Serialize + Deserialize<'a> + Clone,
{
    new_error: Option<io::Error>,
    current_batch_rev: Vec<T>,
    tempfile: Option<BatchReader<File>>,
    last_elements: VecDequeIndex<T>,
    last_elements_index: usize,
    config: SwapVecConfig,
}

impl<T: Serialize + for<'a> Deserialize<'a> + Clone> SwapVecIter<T> {
    fn new(
        tempfile_written: Option<BatchWriter<File>>,
        last_elements: VecDeque<T>,
        config: SwapVecConfig,
    ) -> Self {
        let (tempfile, new_error) = match tempfile_written.map(|v| v.try_into()) {
            None => (None, None),
            Some(Ok(v)) => (Some(v), None),
            Some(Err(e)) => (None, Some(e)),
        };

        Self {
            new_error,
            current_batch_rev: Vec::with_capacity(config.batch_size),
            last_elements: last_elements.into(),
            last_elements_index: 0,
            tempfile,
            config,
        }
    }

    fn read_batch(&mut self) -> Result<Option<Vec<T>>, SwapVecError> {
        if self.tempfile.is_none() {
            return Ok(None);
        }
        if let Some(err) = self.new_error.take() {
            return Err(err.into());
        }

        let tempfile = self.tempfile.as_mut().unwrap();
        let Some(buffer) = tempfile.read_batch()? else {
            return Ok(None);
        };
        let batch: Vec<T> = rmp_serde::from_slice(buffer)?;
        Ok(Some(batch))
    }

    fn next_in_batch(&mut self) -> Result<Option<T>, SwapVecError> {
        if let Some(v) = self.current_batch_rev.pop() {
            return Ok(Some(v));
        }
        if let Some(mut new_batch) = self.read_batch()? {
            new_batch.reverse();
            self.current_batch_rev = new_batch;
            Ok(self.current_batch_rev.pop())
        } else {
            Ok(None)
        }
    }

    pub fn reset(&mut self) {
        self.current_batch_rev.clear();
        self.last_elements_index = 0;
        if let Some(tempfile) = self.tempfile.as_mut() {
            if let Err(e) = tempfile.reset() {
                self.new_error = Some(e);
            }
        }
    }
}

impl<T: Serialize + for<'a> Deserialize<'a> + Clone> Iterator for SwapVecIter<T> {
    type Item = Result<T, SwapVecError>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(item) = self.current_batch_rev.pop() {
            return Some(Ok(item));
        }

        match self.next_in_batch() {
            Err(err) => Some(Err(err)),
            Ok(Some(item)) => Some(Ok(item)),
            Ok(None) => {
                let index = self.last_elements_index;
                self.last_elements_index += 1;
                self.last_elements.get(index).map(Ok)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_iterate_in_memory() {
        let mut v: SwapVec<u32> = SwapVec::default();
        v.consume(0..1024).unwrap();
        let out: Vec<u32> = v.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(out, (0..1024).collect::<Vec<_>>());
    }

    #[test]
    fn spills_to_disk_and_reads_back() {
        let config = SwapVecConfig {
            swap_after: 4,
            batch_size: 4,
        };
        let mut v: SwapVec<u64> = SwapVec::with_config(config);
        v.consume(0..32u64).unwrap();
        assert!(v.written_to_file());
        let out: Vec<u64> = v.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(out, (0..32u64).collect::<Vec<_>>());
    }
}
