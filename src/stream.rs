
use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

const PATIENCE: Duration = Duration::from_secs(20);
pub const PROBE_PATIENCE: Duration = Duration::from_millis(150);

struct State {
    bytes: Vec<u8>,
    finished: bool,
    error: Option<String>,
}

pub struct SharedBuffer {
    state: Mutex<State>,
    signal: Condvar,
    limit: usize,
    total: Option<u64>,
    patience: AtomicU64,
}

impl SharedBuffer {
    pub fn new(limit: usize, total: Option<u64>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                bytes: Vec::new(),
                finished: false,
                error: None,
            }),
            signal: Condvar::new(),
            limit,
            total,
            patience: AtomicU64::new(PATIENCE.as_millis() as u64),
        })
    }

    pub fn push(&self, chunk: &[u8]) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.bytes.len() + chunk.len() > self.limit {
            state.error = Some("Аудиофайл слишком большой".into());
            state.finished = true;
            self.signal.notify_all();
            return false;
        }
        state.bytes.extend_from_slice(chunk);
        self.signal.notify_all();
        true
    }

    pub fn finish(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.finished = true;
            self.signal.notify_all();
        }
    }

    pub fn fail(&self, message: String) {
        if let Ok(mut state) = self.state.lock() {
            state.error = Some(message);
            state.finished = true;
            self.signal.notify_all();
        }
    }

    pub fn set_patience(&self, patience: Duration) {
        self.patience
            .store(patience.as_millis() as u64, Ordering::Relaxed);
    }

    fn patience(&self) -> Duration {
        Duration::from_millis(self.patience.load(Ordering::Relaxed))
    }

    pub fn reader(self: &Arc<Self>) -> Reader {
        Reader {
            buffer: self.clone(),
            position: 0,
        }
    }

    pub fn wait_finished(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        while !state.finished {
            match self.signal.wait_timeout(state, PATIENCE) {
                Ok((next, _)) => state = next,
                Err(_) => return,
            }
        }
    }
}

#[derive(Clone)]
pub struct Reader {
    buffer: Arc<SharedBuffer>,
    position: usize,
}

impl Reader {
    pub fn buffer(&self) -> &Arc<SharedBuffer> {
        &self.buffer
    }

    pub fn rewind(&mut self) {
        self.position = 0;
    }

    fn wait_for(&mut self, needed: usize) -> io::Result<bool> {
        let mut state = self.buffer.state.lock().map_err(lock_error)?;
        let patience = self.buffer.patience();
        let mut last = state.bytes.len();
        loop {
            if state.bytes.len() >= needed {
                return Ok(true);
            }
            if let Some(error) = &state.error {
                return Err(io::Error::other(error.clone()));
            }
            if state.finished {
                return Ok(false);
            }
            let (next, timeout) = self
                .buffer
                .signal
                .wait_timeout(state, patience)
                .map_err(lock_error)?;
            state = next;
            if timeout.timed_out() && state.bytes.len() == last {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Загрузка аудио остановилась",
                ));
            }
            last = state.bytes.len();
        }
    }
}

fn lock_error<T>(_: T) -> io::Error {
    io::Error::other("Буфер аудио недоступен")
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.wait_for(self.position + 1)? {
            return Ok(0);
        }
        let state = self.buffer.state.lock().map_err(lock_error)?;
        let available = state.bytes.len() - self.position;
        let len = buf.len().min(available);
        buf[..len].copy_from_slice(&state.bytes[self.position..self.position + len]);
        self.position += len;
        Ok(len)
    }
}

impl Seek for Reader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let target = match position {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(delta) => {
                let current = self.position as i64;
                (current + delta).max(0) as u64
            }
            SeekFrom::End(delta) => {
                let Some(total) = self.buffer.total else {
                    return Err(io::Error::other("Размер аудиофайла неизвестен"));
                };
                (total as i64 + delta).max(0) as u64
            }
        };
        self.position =
            usize::try_from(target).map_err(|_| io::Error::other("Позиция вне файла"))?;
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::Decoder;
    use std::{io::BufReader, thread};

    fn wav(seconds: u32) -> Vec<u8> {
        let rate = 8000u32;
        let samples = rate * seconds;
        let mut data = Vec::with_capacity(44 + samples as usize * 2);
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36 + samples * 2).to_le_bytes());
        data.extend_from_slice(b"WAVEfmt ");
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&rate.to_le_bytes());
        data.extend_from_slice(&(rate * 2).to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&16u16.to_le_bytes());
        data.extend_from_slice(b"data");
        data.extend_from_slice(&(samples * 2).to_le_bytes());
        for index in 0..samples {
            let value = ((index % 64) as i16 - 32) * 512;
            data.extend_from_slice(&value.to_le_bytes());
        }
        data
    }

    #[test]
    fn reading_waits_for_the_loader() {
        let buffer = SharedBuffer::new(1024, Some(8));
        let mut reader = buffer.reader();
        buffer.push(b"abcd");
        let mut out = [0u8; 8];
        let read = reader.read(&mut out).unwrap();
        assert_eq!(&out[..read], b"abcd");
        let feeder = {
            let buffer = buffer.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(50));
                buffer.push(b"efgh");
                buffer.finish();
            })
        };
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        feeder.join().unwrap();
        assert_eq!(rest, b"efgh");
    }

    #[test]
    fn seek_uses_the_known_length() {
        let buffer = SharedBuffer::new(1024, Some(1000));
        let mut reader = buffer.reader();
        buffer.push(&[7u8; 100]);
        assert_eq!(reader.seek(SeekFrom::Start(10)).unwrap(), 10);
        assert_eq!(reader.seek(SeekFrom::End(0)).unwrap(), 1000);
        assert_eq!(reader.seek(SeekFrom::Current(-30)).unwrap(), 970);

        let unknown = SharedBuffer::new(1024, None);
        let mut reader = unknown.reader();
        assert!(
            reader.seek(SeekFrom::End(0)).is_err(),
            "без длины конец файла неизвестен"
        );
    }

    #[test]
    fn stalled_loading_reports_an_error() {
        let buffer = SharedBuffer::new(1024, Some(10));
        buffer.set_patience(Duration::from_millis(20));
        let mut reader = buffer.reader();
        assert!(reader.read(&mut [0u8; 1]).is_err(), "данных нет — ошибка");

        let failed = SharedBuffer::new(1024, Some(10));
        failed.push(b"x");
        failed.fail("сеть отвалилась".into());
        let mut reader = failed.reader();
        assert!(reader.read(&mut [0u8; 1]).is_ok());
        let error = reader.read(&mut [0u8; 1]).unwrap_err();
        assert!(error.to_string().contains("сеть отвалилась"));
    }

    #[test]
    fn decoder_starts_before_the_file_is_complete() {
        let data = wav(2);
        let total = data.len() as u64;
        let buffer = SharedBuffer::new(1024 * 1024, Some(total));
        let reader = buffer.reader();
        let split = data.len() / 2;
        buffer.push(&data[..split]);
        buffer.set_patience(Duration::from_secs(5));

        let decoder = Decoder::try_from(BufReader::new(reader))
            .unwrap_or_else(|error| panic!("wav не читается: {error}"));
        let feeder = {
            let buffer = buffer.clone();
            let tail = data[split..].to_vec();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(100));
                buffer.push(&tail);
                buffer.finish();
            })
        };
        let decoded = decoder.take(8000 * 2).count();
        feeder.join().unwrap();
        assert_eq!(decoded, 8000 * 2, "второй половины хватило для декодирования");
    }
}
