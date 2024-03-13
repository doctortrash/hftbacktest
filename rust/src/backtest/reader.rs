use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fs::File,
    io::{Error as IoError, ErrorKind, Read},
    marker::PhantomData,
    mem::size_of,
    ops::Index,
    rc::Rc,
};

use crate::{
    backtest::Error,
    ty::{BUY, DEPTH_CLEAR_EVENT, DEPTH_EVENT, DEPTH_SNAPSHOT_EVENT, SELL, TRADE_EVENT},
};

pub const EXCH_EVENT: i64 = 1 << 31;
pub const LOCAL_EVENT: i64 = 1 << 30;

pub const LOCAL_BID_DEPTH_EVENT: i64 = DEPTH_EVENT | BUY | LOCAL_EVENT;
pub const LOCAL_ASK_DEPTH_EVENT: i64 = DEPTH_EVENT | SELL | LOCAL_EVENT;
pub const LOCAL_BID_DEPTH_CLEAR_EVENT: i64 = DEPTH_CLEAR_EVENT | BUY | LOCAL_EVENT;
pub const LOCAL_ASK_DEPTH_CLEAR_EVENT: i64 = DEPTH_CLEAR_EVENT | SELL | LOCAL_EVENT;
pub const LOCAL_BID_DEPTH_SNAPSHOT_EVENT: i64 = DEPTH_SNAPSHOT_EVENT | BUY | LOCAL_EVENT;
pub const LOCAL_ASK_DEPTH_SNAPSHOT_EVENT: i64 = DEPTH_SNAPSHOT_EVENT | SELL | LOCAL_EVENT;

pub const LOCAL_TRADE_EVENT: i64 = TRADE_EVENT | LOCAL_EVENT;
pub const LOCAL_BUY_TRADE_EVENT: i64 = TRADE_EVENT | BUY | LOCAL_EVENT;
pub const LOCAL_SELL_TRADE_EVENT: i64 = TRADE_EVENT | SELL | LOCAL_EVENT;

pub const EXCH_BID_DEPTH_EVENT: i64 = DEPTH_EVENT | BUY | EXCH_EVENT;
pub const EXCH_ASK_DEPTH_EVENT: i64 = DEPTH_EVENT | SELL | EXCH_EVENT;
pub const EXCH_BID_DEPTH_CLEAR_EVENT: i64 = DEPTH_CLEAR_EVENT | BUY | EXCH_EVENT;
pub const EXCH_ASK_DEPTH_CLEAR_EVENT: i64 = DEPTH_CLEAR_EVENT | SELL | EXCH_EVENT;
pub const EXCH_BID_DEPTH_SNAPSHOT_EVENT: i64 = DEPTH_SNAPSHOT_EVENT | BUY | EXCH_EVENT;
pub const EXCH_ASK_DEPTH_SNAPSHOT_EVENT: i64 = DEPTH_SNAPSHOT_EVENT | SELL | EXCH_EVENT;

pub const EXCH_TRADE_EVENT: i64 = TRADE_EVENT | EXCH_EVENT;
pub const EXCH_BUY_TRADE_EVENT: i64 = TRADE_EVENT | BUY | EXCH_EVENT;
pub const EXCH_SELL_TRADE_EVENT: i64 = TRADE_EVENT | SELL | EXCH_EVENT;

pub const WAIT_ORDER_RESPONSE_NONE: i64 = -1;
pub const WAIT_ORDER_RESPONSE_ANY: i64 = -2;

pub const UNTIL_END_OF_DATA: i64 = i64::MAX;

#[derive(Clone, Debug)]
pub struct Data<D> {
    buf: Rc<Vec<u8>>,
    header_len: usize,
    _d_marker: PhantomData<D>,
}

impl<D> Data<D>
where
    D: Sized,
{
    pub fn len(&self) -> usize {
        let size = size_of::<D>();
        (self.buf.len() - self.header_len) / size
    }

    pub fn empty() -> Self {
        Self {
            buf: Default::default(),
            header_len: 0,
            _d_marker: Default::default(),
        }
    }
}

impl<D> Index<usize> for Data<D>
where
    D: Sized,
{
    type Output = D;

    fn index(&self, index: usize) -> &Self::Output {
        let size = size_of::<D>();
        let i = self.header_len + index * size;
        if i + size > self.buf.len() {
            panic!("Out of the size.");
        }
        unsafe { &*(self.buf[i..(i + size)].as_ptr() as *const D) }
    }
}

#[derive(Clone, Debug)]
pub struct Cache<D>(Rc<RefCell<HashMap<String, (Cell<usize>, Data<D>)>>>)
where
    D: Sized;

impl<D> Cache<D>
where
    D: Sized + Clone,
{
    pub fn new() -> Self {
        Self(Default::default())
    }

    pub fn insert(&mut self, key: String, data: Data<D>) {
        self.0.borrow_mut().insert(key, (Cell::new(0), data));
    }

    pub fn remove(&mut self, data: Data<D>) {
        let mut remove = None;
        for (key, (ref_count, cached_data)) in self.0.borrow_mut().iter_mut() {
            if Rc::ptr_eq(&data.buf, &cached_data.buf) {
                *ref_count.get_mut() -= 1;
                if ref_count.get() == 0 {
                    remove = Some(key.clone());
                }
                break;
            }
        }
        if let Some(key) = remove {
            self.0.borrow_mut().remove(&key).unwrap();
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.0.borrow().contains_key(key)
    }

    pub fn get(&mut self, key: &str) -> Data<D> {
        let mut borrowed = self.0.borrow_mut();
        let (ref_count, data) = borrowed.get_mut(key).unwrap();
        *ref_count.get_mut() += 1;
        data.clone()
    }
}

#[derive(Clone, Debug)]
pub struct Reader<D>
where
    D: Sized,
{
    file_list: Vec<String>,
    cache: Cache<D>,
    data_num: usize,
}

impl<D> Reader<D>
where
    D: Sized + Clone,
{
    pub fn new(cache: Cache<D>) -> Self {
        Self {
            file_list: Vec::new(),
            cache,
            data_num: 0,
        }
    }

    pub fn add_file(&mut self, filepath: String) {
        self.file_list.push(filepath);
    }

    pub fn release(&mut self, data: Data<D>) {
        self.cache.remove(data);
    }

    pub fn next(&mut self) -> Result<Data<D>, Error> {
        if self.data_num < self.file_list.len() {
            let filepath = self.file_list.get(self.data_num).unwrap();
            if !self.cache.contains(filepath) {
                if filepath.ends_with(".npy") {
                    let data = read_npy(filepath)?;
                    self.cache.insert(filepath.to_string(), data);
                } else if filepath.ends_with(".npz") {
                    let data = read_npz(filepath)?;
                    self.cache.insert(filepath.to_string(), data);
                } else {
                    return Err(Error::DataError(IoError::new(
                        ErrorKind::InvalidData,
                        "unsupported data type",
                    )));
                }
            }
            let data = self.cache.get(filepath);
            self.data_num += 1;
            Ok(data)
        } else {
            Err(Error::EndOfData)
        }
    }
}

pub fn read_npy<D: Sized>(filepath: &str) -> Result<Data<D>, IoError> {
    let mut file = File::open(filepath)?;

    file.sync_all()?;
    let size = file.metadata()?.len() as usize;
    let mut buf = Vec::with_capacity(size);
    unsafe {
        buf.set_len(size);
    }

    let mut read_size = 0;
    while read_size < size {
        read_size += file.read(&mut buf[read_size..])?;
    }

    let header_len = u16::from_le_bytes(buf[8..10].try_into().unwrap()) as usize;
    // let header = String::from_utf8(buf[10..(10 + header_len)].to_vec()).unwrap().to_string().trim().to_string();

    Ok(Data {
        buf: Rc::new(buf),
        header_len: 10 + header_len,
        _d_marker: Default::default(),
    })
}

pub fn read_npz<D: Sized>(filepath: &str) -> Result<Data<D>, IoError> {
    let mut archive = zip::ZipArchive::new(File::open(filepath)?)?;

    let mut file = archive.by_index(0)?;

    let size = file.size() as usize;
    let mut buf = Vec::with_capacity(size);
    unsafe {
        buf.set_len(size);
    }

    let mut read_size = 0;
    while read_size < size {
        read_size += file.read(&mut buf[read_size..])?;
    }

    let header_len = u16::from_le_bytes(buf[8..10].try_into().unwrap()) as usize;
    // let header = String::from_utf8(buf[10..(10 + header_len)].to_vec()).unwrap().to_string().trim().to_string();

    Ok(Data {
        buf: Rc::new(buf),
        header_len: 10 + header_len,
        _d_marker: Default::default(),
    })
}

// pub fn save_npz<D: Sized>(filepath: &str, data: Data<D>) -> Result<(), IoError> {
//     let mut zip = zip::ZipWriter::new(
//         File::options()
//             .create(true)
//             .write(true)
//             .open(filepath)?
//     )?;
//
//     let options = zip::write::FileOptions::default()
//         .compression_method(zip::CompressionMethod::Zstd);
//
//     let hd = [147u8, 78, 85, 77, 80, 89, 1, 0];
//     let mut header = String::with_capacity(200);
//     header.push_str("{'descr': [('ets', '<i8'), ('lts', '<i8')]");
//     header.push_str(", 'fortran_order': False");
//     header.push_str(", 'shape': (100,), }");
//
//     zip.start_file("arr_0", options)?;
//     zip.write(&hd)?;
//     zip.write(header.to_le_bytes())?;
//     zip.finish();
//
//     Ok(Data {
//         buf: Rc::new(buf),
//         header_len: 10 + header_len,
//         _d: Default::default(),
//     })
// }
