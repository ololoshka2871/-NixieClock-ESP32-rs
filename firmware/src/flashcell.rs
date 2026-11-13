//! Единая flash-ячейка: один экземпляр FlashStorage, доступ к методам через
//! мьютекс. Позволяет NVS и OTA-апдейтеру владеть каждому своим «ручкой»
//! без алиасинга `&mut FlashStorage`. Разовые операции (таблица разделов)
//! используют ту же ручку.
use core::cell::RefCell;

use core::cell::UnsafeCell;

use embassy_sync::blocking_mutex::{
    raw::CriticalSectionRawMutex,
    Mutex,
};
use esp_bootloader_esp_idf::partitions::PARTITION_TABLE_MAX_LEN;
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

/// Общий буфер под таблицу разделов. НЕ StaticCell: with_updater/with_nvs
/// вызываются многократно (boot-ремонты otadata, каждый OTA, каждый
/// save/load настроек), а StaticCell::init паникует при повторной
/// инициализации ("StaticCell is already full") — panic v0.3.0 на буте.
/// Буфер живёт в bss; безопасен, потому что ВСЕ читатели вызываются
/// строго внутри with_flash, т.е. сериализованы flash-мьютексом.
struct SyncBuf(UnsafeCell<[u8; PARTITION_TABLE_MAX_LEN]>);
// SAFETY: обращение к .0 только из замыканий with_flash (под мьютексом).
unsafe impl Sync for SyncBuf {}
static PTBUF: SyncBuf = SyncBuf(UnsafeCell::new([0u8; PARTITION_TABLE_MAX_LEN]));

/// Получить буфер таблицы разделов. Вызывать только из замыкания with_flash.
/// # Safety
/// обращение только под flash-мьютексом (иначе — гонка за &mut).
pub unsafe fn ptbuf() -> &'static mut [u8; PARTITION_TABLE_MAX_LEN] {
    // SAFETY: вызывается только из замыканий with_flash (см. контракт выше).
    unsafe { &mut *PTBUF.0.get() }
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
