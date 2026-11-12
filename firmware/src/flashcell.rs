//! Единая flash-ячейка: один экземпляр FlashStorage, доступ к методам через
//! мьютекс. Позволяет NVS и OTA-апдейтеру владеть каждому своим «ручкой»
//! без алиасинга `&mut FlashStorage`. Разовые операции (таблица разделов)
//! используют ту же ручку.
use core::cell::RefCell;

use embassy_sync::blocking_mutex::{
    raw::CriticalSectionRawMutex,
    Mutex,
};
use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash,
};
use esp_storage::{FlashStorage, FlashStorageError};

/// Zero-sized «ручка» к общей flash: копируется и владеется каждым
/// потребителем (OtaUpdater, Nvs), операции сериализуются.
pub struct SharedFlash;

static FLASH: Mutex<CriticalSectionRawMutex, RefCell<Option<FlashStorage<'static>>>> =
    Mutex::new(RefCell::new(None));

/// Вызвать ровно один раз на старте: положить туда `peripherals.FLASH`.
pub fn init(fs: FlashStorage<'static>) {
    FLASH.lock(|cell| *cell.borrow_mut() = Some(fs));
}

pub fn with_flash<T>(f: impl FnOnce(&mut FlashStorage<'static>) -> T) -> T {
    FLASH.lock(|cell| f(cell.borrow_mut().as_mut().expect("SharedFlash::init not called")))
}

impl ErrorType for SharedFlash {
    type Error = FlashStorageError;
}

impl ReadNorFlash for SharedFlash {
    const READ_SIZE: usize = FlashStorage::READ_SIZE; // 4 (слово)
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        with_flash(|f| f.read_nor(offset, bytes))
    }
    fn capacity(&self) -> usize {
        with_flash(|f| f.capacity())
    }
}

impl NorFlash for SharedFlash {
    const WRITE_SIZE: usize = FlashStorage::WRITE_SIZE; // 4
    const ERASE_SIZE: usize = FlashStorage::ERASE_SIZE; // 4096 (сектор)
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        with_flash(|f| f.write_nor(offset, bytes))
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        with_flash(|f| f.erase(from, to))
    }
}

impl MultiwriteNorFlash for SharedFlash {}

impl esp_nvs::platform::Crc for SharedFlash {
    fn crc32(init: u32, data: &[u8]) -> u32 {
        // esp-hal 1.2.2 не экспортирует rom crc; софтверный CRC esp-nvs
        // совместим с IDF crc32_le (проверено probe-компиляцией NVS).
        esp_nvs::platform::software_crc32(init, data)
    }
}
