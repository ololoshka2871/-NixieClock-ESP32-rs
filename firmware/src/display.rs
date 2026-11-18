//! ЭКСПЕРИМЕНТ, шаг 2a: TG0/TG1/TG2 через I2S1 16-bit parallel + DMA-кольцо,
//! плюс ISR на `OUT_EOF` как тик кадра. Цифры по-прежнему нулевые.
//!
//! Что делает эта версия:
//!   * 16-битный параллельный режим I2S1. Lanes 0..7 — прежние 8 шин
//!     (DATA_8..15, GPIO27/26/25/33/32/12/18/19), lanes 8..10 — TG0/TG1/TG2
//!     (DATA_16..18, GPIO13/2/14), lanes 11..15 — NoPin (в железе их нет).
//!   * Коды ламп (младшие 8 бит слова) — всегда 0. На осциллографе должны
//!     быть видны только три строба.
//!   * Один кадр = 3 слота + хвост, 6400 B. Поле size дескриптора — 12 бит
//!     (максимум 4095 B), поэтому кадр разбит на 2 дескриптора по 3200 B.
//!     `suc_eof` стоит только на втором — один тик на кадр.
//!   * 3 кадра (6 дескрипторов) замкнуты в кольцо. После `send()` DMA обходит
//!     кольцо вечно, процессор в тракте не участвует.
//!   * ISR на `I2S1` по `OUT_EOF`: считает тики кадра и запоминает
//!     `OUT_EOF_DES_ADDR`, чтобы (а) подтвердить частоту ~397 Гц и (б) понять,
//!     какой дескриптор железо называет «только что законченным». Это
//!     фундамент для N-буферизации: по этому адресу выбирается кадр, который
//!     DMA ещё не читает.
//!
//! Замерено на шаге 1: слово = 787 нс (1.27 Мслов/с), то есть в 16-bit режиме
//! при `SAMPLE_HZ = 1.27 МГц` на слово приходится 2 BCK. Делитель упирается в
//! максимум 63 (минимум частоты на ESP32), поэтому медленнее слово не пойдёт.
//! Отсюда геометрия:
//!   * слот 1056 слов = 831.5 мкс (TG высокий 1024 слова = 806 мкс),
//!   * кадр 3200 слов = 2519.7 мкс = 396.9 Гц.
//!
//! Что проверить на этом шаге:
//!   1. Картинка на осциллографе не изменилась (слот 831 мкс, кадр 2.52 мс).
//!   2. В логе раз в 2 с: `eof` растёт на ~794 за 2 с (`~397 Hz`), `desc +1/+3/+5`
//!      (или +2/+4/+0 — зависит от того, отдаёт ли железо адрес самого
//!      EOF-дескриптора или следующего за ним; это и выясняем).
//!   3. `bad=0`, `tx_idle=false`.
//!
//! Оригинальная реализация сохранена рядом: `display.rs.bak`.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use embassy_executor::Spawner;
use embassy_time::{Instant, Timer};

use esp_hal::dma::{BurstConfig, DmaDescriptor, DmaTxBuffer, Preparation};
use esp_hal::gpio::NoPin;
use esp_hal::i2s::parallel::{I2sParallel, TxSixteenBits};
use esp_hal::interrupt::{InterruptHandler, Priority, bind_handler};
use esp_hal::peripherals;
use esp_hal::peripherals::Interrupt;
use esp_hal::time::Rate;

use crate::hangdetect;

/// Частота, которую получает драйвер. Для 16-bit режима esp-hal считает
/// mclk = 2 * SAMPLE_HZ, bck_div = 1; при 1.27 МГц делитель упирается в
/// максимум 63 (минимум частоты, который вообще можно выставить на ESP32).
/// Замер: одно 16-битное слово = 787 нс.
const SAMPLE_HZ: u32 = 1_270_000;

/// Слово = 16 бит = одна посылка на шину. Биты 0..7 — коды ламп, 8..10 — TG.
const TG_SHIFT: u16 = 8;

/// Слот: пауза, окно строба, пауза. Всё в 16-битных словах.
const SETUP_WORDS: usize = 16;
const ON_WORDS: usize = 1024;
const HOLD_WORDS: usize = 16;
const SLOT_WORDS: usize = SETUP_WORDS + ON_WORDS + HOLD_WORDS; // 1056
/// Хвост кадра: гарантированно тёмное состояние после третьего слота.
const TAIL_WORDS: usize = 32;
const FRAME_WORDS: usize = SLOT_WORDS * 3 + TAIL_WORDS; // 3200
const FRAME_U32: usize = FRAME_WORDS / 2; // DMA пересылает 32-битные посылки
const FRAME_BYTES: usize = FRAME_WORDS * 2; // 6400 B
/// Поле size/length дескриптора — 12 бит, поэтому кадр не влезает целиком.
const DESC_PER_FRAME: usize = 2;
const DESC_BYTES: usize = FRAME_BYTES / DESC_PER_FRAME; // 3200 B < 4096
const DESC_COUNT: usize = FRAME_COUNT * DESC_PER_FRAME;

/// Сколько кадров в кольце. 3 — как просили; для боевого режима захочется 4+.
const FRAME_COUNT: usize = 3;

/// Стендовые тумблеры. На этом шаге не используются (данные всегда 0),
/// но остаются, чтобы не менять API сервера.
pub static TEST_MODE: AtomicU8 = AtomicU8::new(0);
pub static DISPLAY_ON: AtomicBool = AtomicBool::new(true);
pub static NO_BLANK: AtomicBool = AtomicBool::new(false);

/// Слово кадра: код ламп (0) + строб слота. `slot >= 3` — тёмный хвост.
fn word_at(word_index: usize) -> u16 {
    let slot = word_index / SLOT_WORDS;
    if slot >= 3 {
        return 0;
    }
    let off = word_index % SLOT_WORDS;
    let strobe_on = off >= SETUP_WORDS && off < SETUP_WORDS + ON_WORDS;
    if strobe_on {
        1 << (TG_SHIFT as usize + slot)
    } else {
        0
    }
}

/// Заполняет один кадр. В памяти лежат 32-битные посылки по два 16-битных
/// слова; esp-hal документирует перестановку `[A,B,C,D] -> [B,A,D,C]`, поэтому
/// дублируем слово в обе половины посылки — порядок внутри пары перестаёт
/// иметь значение.
fn fill_frame(frame: &mut [u32; FRAME_U32]) {
    for (beat, cell) in frame.iter_mut().enumerate() {
        let w = word_at(beat * 2) as u32;
        *cell = w | (w << 16);
    }
}

/// Кольцо DMA: дескрипторы + кадры. Живёт в статике, потому что DMA читает
/// эту память после того, как задача отдала дескрипторы драйверу.
struct Ring {
    descs: [DmaDescriptor; DESC_COUNT],
    frames: [[u32; FRAME_U32]; FRAME_COUNT],
}

impl Ring {
    const fn new() -> Self {
        Self {
            descs: [DmaDescriptor::EMPTY; DESC_COUNT],
            frames: [[0; FRAME_U32]; FRAME_COUNT],
        }
    }

    /// Раскладывает кадры и замыкает дескрипторы в кольцо.
    fn build(&mut self) {
        for frame in &mut self.frames {
            fill_frame(frame);
        }

        let base = self.descs.as_mut_ptr();
        for i in 0..DESC_COUNT {
            let frame = i / DESC_PER_FRAME;
            let part = i % DESC_PER_FRAME;
            // Сырые указатели: собрать кольцо через &mut заимствования нельзя.
            let next = unsafe { base.add((i + 1) % DESC_COUNT) };
            let buf = unsafe {
                self.frames[frame]
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(part * DESC_BYTES)
            };
            let d = unsafe { &mut *base.add(i) };
            // owner = DMA; EOF только на последнем дескрипторе кадра.
            d.reset_for_tx(part == DESC_PER_FRAME - 1);
            d.set_size(DESC_BYTES);
            d.set_length(DESC_BYTES);
            d.buffer = buf;
            d.next = next;
        }
    }

    fn start(&mut self) -> RingStart {
        RingStart {
            start: self.descs.as_mut_ptr(),
        }
    }
}

/// Кольцо лежит в .bss сразу, а не инициализируется `StaticCell::init(Ring::new())`:
/// `init` принимает значение по значению, и 19 KB временного `Ring` строились
/// на стеке задачи (кадр 38 688 B при стеке 30 432 B) -> stack guard panic.
/// Доступ только из `display_task`, которая запускается ровно один раз.
static mut RING: Ring = Ring::new();

/// Диагностика ISR: тики кадра и адрес последнего дескриптора с EOF.
static EOF_TICKS: AtomicU32 = AtomicU32::new(0);
static EOF_DESC: AtomicU32 = AtomicU32::new(0);
static RING_BASE: AtomicU32 = AtomicU32::new(0);
static EOF_BAD_ADDR: AtomicU32 = AtomicU32::new(0);

/// Тик кадра по `OUT_EOF`. Обработчик только читает `OUT_EOF_DES_ADDR`,
/// обновляет счётчики и сбрасывает флаг: DMA не трогает и данные не меняет.
/// `.rwtext` = IRAM: тик идёт и во время записи flash (OTA), когда кэш выключен.
#[unsafe(link_section = ".rwtext")]
extern "C" fn i2s1_eof_isr() {
    let regs = unsafe { &*esp32::I2S1::ptr() };
    if !regs.int_raw().read().out_eof().bit_is_set() {
        return;
    }
    // Адрес читаем до очистки флага — он относится именно к этому событию.
    let addr = regs.out_eof_des_addr().read().bits();
    let off = addr.wrapping_sub(RING_BASE.load(Ordering::Relaxed)) as usize;
    if off >= DESC_COUNT * core::mem::size_of::<DmaDescriptor>() {
        EOF_BAD_ADDR.fetch_add(1, Ordering::Relaxed);
    }
    EOF_DESC.store(addr, Ordering::Relaxed);
    EOF_TICKS.fetch_add(1, Ordering::Relaxed);
    regs.int_clr().write(|w| w.out_eof().clear_bit_by_one());
}

/// Обёртка для `I2sParallel::send`: сам буфер остаётся в статике, сюда кладём
/// только указатель на голову кольца.
struct RingStart {
    start: *mut DmaDescriptor,
}

// SAFETY: RingStart живёт в статике задачи; дескрипторы и кадры не двигаются
// и не освобождаются, пока трансфер (и задача) живы.
unsafe impl DmaTxBuffer for RingStart {
    type View = ();
    type Final = ();

    fn prepare(&mut self) -> Preparation {
        Preparation {
            start: self.start,
            // Ровно как DmaLoopBuf: владелец не проверяется, write-back выключен.
            burst_transfer: BurstConfig::default(),
            check_owner: Some(false),
            auto_write_back: false,
        }
    }

    fn into_view(self) {}
    fn from_view(_view: Self::View) {}
}

/// Экспериментальная задача: собрать кольцо, запустить DMA и больше его не
/// трогать. Логика часов вернётся на следующем шаге.
#[embassy_executor::task]
pub async fn display_task(
    i2s: peripherals::I2S1<'static>,
    dma: peripherals::DMA_I2S1<'static>,
    pins: TxSixteenBits<'static>,
    ws: peripherals::GPIO3<'static>,
) -> ! {
    // SAFETY: display_task — единственный пользователь RING (запускается один
    // раз), поэтому эксклюзивная ссылка корректна. `addr_of_mut!` не создаёт
    // промежуточной ссылки на `static mut`.
    let ring = unsafe { &mut *core::ptr::addr_of_mut!(RING) };
    ring.build();

    log::info!(
        "display(exp): ring {} frames x {} words ({} B), {} desc x {} B, TG on lanes 8..10",
        FRAME_COUNT,
        FRAME_WORDS,
        FRAME_BYTES,
        DESC_COUNT,
        DESC_BYTES,
    );
    log::info!(
        "display(exp): expected slot {}.{} us, frame {}.{} us (~{} Hz)",
        831,
        5,
        2519,
        7,
        396
    );

    let start = ring.start();
    let par = I2sParallel::new(i2s, dma, Rate::from_hz(SAMPLE_HZ), pins, ws);

    let xfer = match par.send(start) {
        Ok(xfer) => xfer,
        Err((e, _par, _buf)) => {
            log::error!("display(exp): circular DMA start failed: {:?}", e);
            loop {
                Timer::after_secs(1).await;
            }
        }
    };

    log::info!("display(exp): circular DMA started, CPU is out of the data path");

    // OUT_EOF = конец дескриптора с `suc_eof`, то есть один тик на кадр.
    // Порядок важен: `ChannelTx::new` погасил линию I2S1 на всех CPU, поэтому
    // сначала чистим возможный pending и разрешаем источник, а `bind_handler`
    // последним снова разрешает линию. `int_ena` недоступен через esp-hal —
    // пишем через PAC.
    RING_BASE.store(ring.descs.as_mut_ptr() as usize as u32, Ordering::Relaxed);
    let regs = unsafe { &*esp32::I2S1::ptr() };
    regs.int_clr().write(|w| w.out_eof().clear_bit_by_one());
    regs.int_ena().modify(|_, w| w.out_eof().set_bit());
    bind_handler(
        Interrupt::I2S1,
        InterruptHandler::new(i2s1_eof_isr, Priority::Priority1),
    );
    log::info!("display(exp): OUT_EOF armed (frame tick, ISR at Priority1)");

    hangdetect::beat(&hangdetect::FRAME);

    let mut tick: u32 = 0;
    let mut last = Instant::now();
    let mut last_ticks = EOF_TICKS.load(Ordering::Relaxed);
    let mut last_desc = 0u32;
    loop {
        Timer::after_millis(250).await;
        hangdetect::beat(&hangdetect::FRAME);
        tick += 1;
        if tick % 8 == 0 {
            let now = Instant::now();
            let elapsed_us = (now - last).as_micros().max(1) as u64;
            let ticks = EOF_TICKS.load(Ordering::Relaxed);
            let addr = EOF_DESC.load(Ordering::Relaxed);
            let hz = (ticks.wrapping_sub(last_ticks)) as u64 * 1_000_000 / elapsed_us;
            let desc = addr.wrapping_sub(RING_BASE.load(Ordering::Relaxed))
                / core::mem::size_of::<DmaDescriptor>() as u32;
            // desc должно ползти 1,3,5 (или 2,4,0) — тогда тик = кадр.
            log::info!(
                "display(exp): alive, tx_idle={}, eof {} (~{} Hz), desc +{} (prev +{}), bad={}, test={}, on={}",
                xfer.is_done(),
                ticks,
                hz,
                desc,
                last_desc,
                EOF_BAD_ADDR.load(Ordering::Relaxed),
                TEST_MODE.load(Ordering::Relaxed),
                DISPLAY_ON.load(Ordering::Relaxed),
            );
            last = now;
            last_ticks = ticks;
            last_desc = desc;
        }
    }
}

/// Claimed once from main: builds the 16-bit pin bundle and spawns the task.
pub fn spawn(
    spawner: &Spawner,
    i2s: peripherals::I2S1<'static>,
    dma: peripherals::DMA_I2S1<'static>,
    gpio27: peripherals::GPIO27<'static>,
    gpio26: peripherals::GPIO26<'static>,
    gpio25: peripherals::GPIO25<'static>,
    gpio33: peripherals::GPIO33<'static>,
    gpio32: peripherals::GPIO32<'static>,
    gpio12: peripherals::GPIO12<'static>,
    gpio18: peripherals::GPIO18<'static>,
    gpio19: peripherals::GPIO19<'static>,
    ws: peripherals::GPIO3<'static>,
    tg13: peripherals::GPIO13<'static>,
    tg2: peripherals::GPIO2<'static>,
    tg14: peripherals::GPIO14<'static>,
) {
    // 16-bit режим: lane i -> I2S1O_DATA_(8+i).
    // lanes 0..7   = шины ламп (DATA_8..15)
    // lanes 8..10  = TG0/TG1/TG2 (DATA_16..18)
    // lanes 11..15 = физически отсутствуют
    let pins = TxSixteenBits::new(
        gpio27, gpio26, gpio25, gpio33, gpio32, gpio12, gpio18, gpio19, tg13, tg2, tg14, NoPin,
        NoPin, NoPin, NoPin, NoPin,
    );
    spawner
        .spawn(display_task(i2s, dma, pins, ws).unwrap());
}
