//! Индикация: I2S1 16-bit parallel + DMA-кольцо, один кадр в железе и
//! атомарная подмена цифр по тику кадра.
//!
//! Раскладка шин: lanes 0..7 — восемь шин ламп (DATA_8..15,
//! GPIO27/26/25/33/32/12/18/19), lanes 8..10 — TG0/TG1/TG2 (DATA_16..18,
//! GPIO13/2/14), lanes 11..15 — NoPin. В 16-битном слове биты 0..7 — коды
//! дешифраторов, биты 8..10 — стробы слотов.
//!
//! Почему один кадр, а не N целых кадров. Кадр = 3 слота (по 1056 слов) +
//! 32 слова тьмы = 3200 слов (2.52 мс, ~397 Гц). Если держать в кольце
//! 3 полных кадра, то обновлять их можно только по одному (пока DMA читает
//! один, остальные свободны), и на время обновления дисплей показывал бы
//! смесь старых и новых цифр. Вместо этого окно строба собрано из 8
//! дескрипторов по 128 слов, которые смотрят в один и тот же маленький
//! буфер слота (256 B). Тогда:
//!
//!   * кадр один и крутится вечно: dark(32) + ON(8x128) + dark(32) + ... +
//!     tail-dark(32), 28 дескрипторов;
//!   * цифры живут в трёх буферах по 256 B (slot 0..2), а не в 6.4 KB;
//!   * ISR на `OUT_EOF` (конец tail-dark) получает готовые семплы и
//!     переписывает только изменившиеся буферы — обычно один слот, 64
//!     u32-записи, ~1-2 мкс. До первого ON-слова следующего кадра остаётся
//!     32 слова тьмы (25 мкс), но slot 0 обычно не меняется, а slot 2
//!     (секунды) читается через ~1.7 мс — запас огромный;
//!   * кадр всегда целостный: старый и новый кадр не смешиваются никогда,
//!     потому что подмена идёт на границе кадра, а не посреди него.
//!
//! Память: в `.bss` ~1.2 KB вместо 19 KB (стек main-задачи был 30 KB, станет
//! ~48 KB). Линкер кладёт `_stack_end` сразу после `.bss`, так что каждый
//! килобайт статики = килобайт стека.
//!
//! Яркость. Окно строба — это один и тот же 128-словный буфер, прочитанный
//! 8 раз, поэтому ШИМ встроен в сами слова: лампа A горит первые `duty_a`
//! слов блока, лампа B — первые `duty_b` (независимо). Частота ШИМ
//! ≈ 1.27 МГц / 128 ≈ 9.9 кГц, 128 градаций. Плавная смена цифры — это
//! линейное уменьшение duty до 0, подмена кода и линейный подъём обратно;
//! старый и новый код никогда не горят в одном кадре.
//!
//! Замерено: слово = 787 нс (1.27 Мслов/с, делитель 160 МГц / 2.54 МГц = 63 —
//! минимум для 6-битного поля). Отсюда слот 1056 слов = 831.5 мкс (строб
//! 1024 слова = 806 мкс), кадр 3200 слов = 2519.7 мкс = 396.9 Гц.
//!
//! Оригинальная 8-битная реализация сохранена рядом: `display.rs.bak`.

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

use clock_core::display as display_core;

use crate::{clock, hangdetect};

/// Частота, которую получает драйвер. Для 16-bit режима esp-hal считает
/// mclk = 2 * SAMPLE_HZ, bck_div = 1; при 1.27 МГц делитель упирается в
/// максимум 63 (минимум частоты, который вообще можно выставить на ESP32).
/// Замер: одно 16-битное слово = 787 нс.
const SAMPLE_HZ: u32 = 1_270_000;

/// Слово = 16 бит. Биты 0..7 — коды ламп, 8..10 — TG0/TG1/TG2.
const TG_SHIFT: u16 = 8;

/// Тёмная пауза перед слотом и в конце кадра. 32 слова = 25.2 мкс.
const DARK_WORDS: usize = 32;
/// Тёмное слово: оба дешифратора в гашении (код 10 = 0xA), все TG = 0.
///
/// Раскладка 16-битного слова: биты 0..3 — код шины A, 4..7 — код шины B,
/// **8..10 — TG0/TG1/TG2**, 11..15 — NoPin. Значит гашение это
/// `0x00AA` (0xA в каждом ниббле, старший байт нулевой).
///
/// Здесь стояло `0x0A0A` — и это была ошибка: `0x0A` в старшем байте
/// занимает биты 8..15, то есть поднимала **бит 9 (TG1)** и обнуляла код
/// шины B. Осциллограф это и показал: TG1 высокий всю тёмную паузу
/// (25 мкс перед слотом, 50 мкс на границе кадра), а на лампах слота 1
/// в это время висел код 0 («9») с яркостью ~1%. Было ещё `0xAAAA` —
/// там коды верные, но бит 9 тоже поднят.
const DARK_WORD_U32: u32 = 0x00AA_00AA;
/// Компиляторная страховка: в тёмном слове не должно быть ни одного строба
/// (биты 8..10 в каждой половине) и оба ниббла должны быть кодом гашения.
const _: () = assert!(DARK_WORD_U32 & 0x0700_0700 == 0, "TG в тёмном слове!");
const _: () = assert!(
    DARK_WORD_U32 & 0x00FF == 0x00AA && DARK_WORD_U32 >> 16 & 0x00FF == 0x00AA,
    "код гашения в тёмном слове!"
);
/// Окно строба: 1024 слова = 806 мкс.
const ON_WORDS: usize = 1024;
/// Один дескриптор окна = 128 слов (256 B). Восемь таких дескрипторов
/// смотрят в один буфер слота — так цифры занимают 256 B вместо 2 KB.
const ON_CHUNK_WORDS: usize = 128;
const ON_CHUNKS: usize = ON_WORDS / ON_CHUNK_WORDS; // 8

const SLOT_WORDS: usize = DARK_WORDS + ON_WORDS; // 1056
const FRAME_WORDS: usize = SLOT_WORDS * 3 + DARK_WORDS; // 3200

/// Скважность PWM задаётся числом горящих слов в повторяющемся блоке.
/// Блок — ровно `ON_CHUNK_WORDS`; если разъедется, яркость 128 перестанет
/// быть «полной» и начнёт мигать.
const _: () = assert!(display_core::DUTY_MAX as usize == ON_CHUNK_WORDS);

/// DMA пересылает 32-битные посылки (два 16-битных слова в каждой).
const DARK_U32: usize = DARK_WORDS / 2; // 16
const ON_U32: usize = ON_CHUNK_WORDS / 2; // 64

const DARK_BYTES: usize = DARK_WORDS * 2; // 64 B
const ON_BYTES: usize = ON_CHUNK_WORDS * 2; // 256 B

/// На слот: один dark-дескриптор + 8 ON-дескрипторов; плюс хвостовой dark.
const DESC_PER_SLOT: usize = 1 + ON_CHUNKS; // 9
const DESC_COUNT: usize = DESC_PER_SLOT * 3 + 1; // 28

/// Стендовые тумблеры (эндпоинты /display/*).
pub static TEST_MODE: AtomicU8 = AtomicU8::new(0);
/// Маска теста: 0..5 — показывать цифру только на этой лампе, 6 — на всех.
pub static TEST_LAMP: AtomicU8 = AtomicU8::new(6);
/// Тестовый режим «только шина»: коды идут, стробы TG закрыты (лампы тёмные).
pub static NO_STROBE: AtomicBool = AtomicBool::new(false);
/// Индикация включена по умолчанию: после бута сразу показываем время.
/// `/display/off` гасит лампы (и `/display/test?...` включает обратно),
/// `/display/test?off=1` возвращает часы.
pub static DISPLAY_ON: AtomicBool = AtomicBool::new(true);
/// Плавная смена цифр (fade): линейно вниз до нуля, затем вверх с новым кодом.
/// По умолчанию включена; `/display/fade?on=0` выключает (мгновенная смена),
/// `/display/fade?on=1` включает обратно. Живёт в RAM: после сброса снова вкл.
pub static FADE_ENABLED: AtomicBool = AtomicBool::new(true);
/// Оставлен для совместимости API: в новой схеме «пустых» чанков нет,
/// тьма между слотами зашита в сами дескрипторы.
pub static NO_BLANK: AtomicBool = AtomicBool::new(false);

// ---------------- заявка на новые цифры и яркость ----------------
//
// На каждый слот приходится два разряда (лампа g — младший ниббл, лампа
// g+3 — старший). Задача пишет упакованное состояние в SLOT_REQ[g]:
//
//   биты 0..7   — код лампы A (g)       биты 8..15  — её скважность 0..128
//   биты 16..23 — код лампы B (g+3)     биты 24..31 — её скважность 0..128
//
// Скважность 128 = 128 слов из 128-словного блока PWM (полная яркость),
// 0 = лампа погашена. Блок повторяется 8 раз внутри окна строба, поэтому
// ШИМ идёт на ~9.9 кГц и не мерцает.
//
// REQ_SEQ — seqlock: нечётное значение означает «задача пишет», ISR в этот
// кадр заявку пропускает (лучше пропустить тик, чем показать смесь старого
// и нового). После записи счётчик снова чётный.
const REQ_FLAGS_OFF: u32 = 1 << 0;
const REQ_FLAGS_NOSTROBE: u32 = 1 << 1;

static SLOT_REQ: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];
static REQ_FLAGS: AtomicU32 = AtomicU32::new(REQ_FLAGS_OFF);
static REQ_SEQ: AtomicU32 = AtomicU32::new(0);
/// Что уже записано в буферы слотов (для ISR). u32::MAX — «ещё ничего».
static APPLIED_SLOT: [AtomicU32; 3] = [const { AtomicU32::new(u32::MAX) }; 3];
static APPLIED_FLAGS: AtomicU32 = AtomicU32::new(u32::MAX);

// ---------------- диагностика ----------------
static EOF_TICKS: AtomicU32 = AtomicU32::new(0);
static EOF_DESC: AtomicU32 = AtomicU32::new(0);
static RING_BASE: AtomicU32 = AtomicU32::new(0);
static EOF_BAD_ADDR: AtomicU32 = AtomicU32::new(0);
static APPLIED: AtomicU32 = AtomicU32::new(0);
static REQUESTS: AtomicU32 = AtomicU32::new(0);

/// Кольцо DMA: дескрипторы + маленькие буферы слотов. Живёт в .bss сразу,
/// без `StaticCell::init(Ring::new())`: `init` принимает значение по значению,
/// и временный `Ring` строился бы на стеке задачи.
struct Ring {
    descs: [DmaDescriptor; DESC_COUNT],
    /// Общий буфер всех тёмных промежутков (4 дескриптора).
    dark: [u32; DARK_U32],
    /// Буфер окна строба на каждый слот: 128 слов ШИМ-картинки, которую
    /// восемь дескрипторов читают по кругу.
    on: [[u32; ON_U32]; 3],
}

impl Ring {
    const fn new() -> Self {
        Self {
            descs: [DmaDescriptor::EMPTY; DESC_COUNT],
            dark: [DARK_WORD_U32; DARK_U32],
            on: [[DARK_WORD_U32; ON_U32]; 3],
        }
    }

    /// Раскладывает дескрипторы: dark, 8xON, dark, 8xON, dark, 8xON, tail.
    /// `suc_eof` — только на хвостовом: один тик на кадр.
    fn build(&mut self) {
        let base = self.descs.as_mut_ptr();
        let dark = self.dark.as_mut_ptr().cast::<u8>();
        for i in 0..DESC_COUNT {
            let (buf, size, eof) = if i == DESC_COUNT - 1 {
                (dark, DARK_BYTES, true)
            } else {
                let slot = i / DESC_PER_SLOT;
                let part = i % DESC_PER_SLOT;
                if part == 0 {
                    (dark, DARK_BYTES, false)
                } else {
                    (self.on[slot].as_mut_ptr().cast::<u8>(), ON_BYTES, false)
                }
            };
            // Сырые указатели: собрать кольцо через &mut заимствования нельзя.
            let next = unsafe { base.add((i + 1) % DESC_COUNT) };
            let d = unsafe { &mut *base.add(i) };
            d.reset_for_tx(eof); // owner = DMA
            d.set_size(size);
            d.set_length(size);
            d.buffer = buf;
            d.next = next;
        }
    }

    /// Заполняет буфер слота ШИМ-картинкой: лампа A горит первые `duty_a`
    /// слов из 128, лампа B — первые `duty_b`. Блок повторяется 8 раз, так
    /// что средняя яркость каждой лампы = duty/128 независимо от соседней.
    ///
    /// `inline(always)` обязателен: метод вызывается из ISR, а ISR живёт в
    /// `.rwtext` (IRAM); отдельная копия в flash не переживёт запись OTA.
    /// Запись через `write_volatile` в цикле, а не `fill`: никакого вызова
    /// ROM-memset из прерывания, ровно 64 32-битных store.
    #[inline(always)]
    fn set_slot_pwm(
        &mut self,
        slot: usize,
        code_a: u8,
        duty_a: u8,
        code_b: u8,
        duty_b: u8,
        strobe: bool,
    ) {
        let tg = if strobe {
            1u16 << (TG_SHIFT as usize + slot)
        } else {
            0
        };
        for (i, cell) in self.on[slot].iter_mut().enumerate() {
            // Один u32 = два 16-битных слова канала I2S.
            let lo = display_core::pwm_sample((i * 2) as u8, code_a, duty_a, code_b, duty_b)
                as u16
                | tg;
            let hi = display_core::pwm_sample((i * 2 + 1) as u8, code_a, duty_a, code_b, duty_b)
                as u16
                | tg;
            unsafe { core::ptr::write_volatile(cell, lo as u32 | (hi as u32) << 16) };
        }
    }

    /// Полностью погасить все стробы (данные при этом не важны).
    #[inline(always)]
    fn blank_all(&mut self) {
        for block in &mut self.on {
            for cell in block.iter_mut() {
                unsafe { core::ptr::write_volatile(cell, DARK_WORD_U32) };
            }
        }
    }

    fn start(&mut self) -> RingStart {
        RingStart {
            start: self.descs.as_mut_ptr(),
        }
    }
}

/// Кольцо в .bss сразу: доступ только из задачи при старте и из ISR по
/// границе кадра. Ссылка из задачи живёт только до `send()`.
static mut RING: Ring = Ring::new();

/// Тик кадра по `OUT_EOF`: читает `OUT_EOF_DES_ADDR`, обновляет счётчики и,
/// если есть заявка, переписывает буферы слотов ДО начала следующего кадра.
///
/// Почему это безопасно: `suc_eof` стоит на хвостовом dark-дескрипторе, так
/// что ISR просыпается ровно на границе кадра. DMA в этот момент начинает
/// 32 слова тьмы перед slot 0; буферы слотов никто не читает.
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

    // Заявка читается через seqlock: пока задача пишет слоты, счётчик
    // нечётный — кадр пропускаем, чтобы не собрать полу-новое состояние.
    let seq = REQ_SEQ.load(Ordering::Acquire);
    if seq & 1 == 0 {
        let flags = REQ_FLAGS.load(Ordering::Relaxed);
        let slots = [
            SLOT_REQ[0].load(Ordering::Relaxed),
            SLOT_REQ[1].load(Ordering::Relaxed),
            SLOT_REQ[2].load(Ordering::Relaxed),
        ];
        if REQ_SEQ.load(Ordering::Acquire) == seq {
            let applied_flags = APPLIED_FLAGS.load(Ordering::Relaxed);
            let mut changed = flags != applied_flags;
            for slot in 0..3 {
                if slots[slot] != APPLIED_SLOT[slot].load(Ordering::Relaxed) {
                    changed = true;
                }
            }
            if changed {
                let ring = unsafe { &mut *core::ptr::addr_of_mut!(RING) };
                let off = flags & REQ_FLAGS_OFF != 0;
                let strobe = flags & REQ_FLAGS_NOSTROBE == 0;
                if off {
                    ring.blank_all();
                    // Слоты при OFF неважны, но запоминаем их как «применённые»:
                    // иначе каждый следующий кадр видел бы расхождение и зря
                    // переписывал все буферы (при включении флаги всё равно
                    // изменятся и заставят переписать слоты заново).
                    for slot in 0..3 {
                        APPLIED_SLOT[slot].store(slots[slot], Ordering::Relaxed);
                    }
                } else {
                    // slot 0 пишем первым: до его окна остаётся меньше всего
                    // времени; остальные слоты читаются на ~0.85/1.7 мс позже.
                    for slot in 0..3 {
                        // При смене только флагов (например, nostrobe) буферы
                        // тоже надо переписать — в них зашит бит строба.
                        if slots[slot] != APPLIED_SLOT[slot].load(Ordering::Relaxed)
                            || flags != applied_flags
                        {
                            let w = slots[slot];
                            ring.set_slot_pwm(
                                slot,
                                (w & 0xFF) as u8,
                                (w >> 8) as u8 & 0xFF,
                                (w >> 16) as u8,
                                (w >> 24) as u8 & 0xFF,
                                strobe,
                            );
                            APPLIED_SLOT[slot].store(w, Ordering::Relaxed);
                        }
                    }
                }
                APPLIED_FLAGS.store(flags, Ordering::Relaxed);
                APPLIED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    regs.int_clr().write(|w| w.out_eof().clear_bit_by_one());
}

/// Обёртка для `I2sParallel::send`: сам буфер остаётся в статике, сюда кладём
/// только указатель на голову кольца.
struct RingStart {
    start: *mut DmaDescriptor,
}

// SAFETY: RingStart живёт в статике; дескрипторы и буферы не двигаются и не
// освобождаются, пока трансфер (и задача) живы.
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

/// Что показать в текущей секунде: тест-режим или реальное время.
/// `lamp` = Some(0..5) в тест-режиме оставляет цифру только на одной лампе.
fn wanted_samples(mode: u8, epoch: u64, lamp: Option<u8>) -> [u8; 3] {
    match display_core::test_samples_lamp(mode, epoch, lamp) {
        Some(s) => s,
        None => {
            let c = clock_core::time::epoch_to_civil(epoch);
            display_core::slot_samples(&display_core::hhmmss_digits(c.hour, c.min, c.sec))
        }
    }
}

/// 0..5 — конкретная лампа, всё остальное (в т.ч. 6) — все шесть.
fn lamp_of(v: u8) -> Option<u8> {
    if v < 6 { Some(v) } else { None }
}

/// Раскладка трёх байт-семплов на шесть кодов ламп:
/// слот g несёт лампу g (младший ниббл) и лампу g+3 (старший).
fn lamp_codes(samples: [u8; 3]) -> [u8; 6] {
    core::array::from_fn(|l| {
        let b = samples[l % 3];
        if l < 3 { b & 0x0F } else { b >> 4 }
    })
}

/// Упаковка состояния шести ламп в три слова для ISR (см. SLOT_REQ).
fn slot_words(fades: &[display_core::LampFade; 6]) -> [u32; 3] {
    core::array::from_fn(|slot| {
        let a = &fades[slot];
        let b = &fades[slot + 3];
        a.code() as u32
            | (a.duty() as u32) << 8
            | (b.code() as u32) << 16
            | (b.duty() as u32) << 24
    })
}

fn flags_now(off: bool, nostrobe: bool) -> u32 {
    if off {
        REQ_FLAGS_OFF
    } else if nostrobe {
        REQ_FLAGS_NOSTROBE
    } else {
        0
    }
}

/// Публикует состояние слотов одной транзакцией seqlock.
fn publish(words: &[u32; 3], flags: u32) {
    let seq = REQ_SEQ.load(Ordering::Relaxed);
    REQ_SEQ.store(seq.wrapping_add(1), Ordering::Release); // нечётный = идёт запись
    for (slot, w) in words.iter().enumerate() {
        SLOT_REQ[slot].store(*w, Ordering::Relaxed);
    }
    REQ_FLAGS.store(flags, Ordering::Relaxed);
    REQ_SEQ.store(seq.wrapping_add(2), Ordering::Release); // чётный = готово
}

/// Текущая яркость каждой лампы (0..=DUTY_MAX) для `/status` и WebUI.
pub fn lamp_duties() -> [u8; 6] {
    let mut out = [0u8; 6];
    for slot in 0..3 {
        let w = SLOT_REQ[slot].load(Ordering::Relaxed);
        out[slot] = (w >> 8) as u8;
        out[slot + 3] = (w >> 24) as u8;
    }
    out
}

/// Задача индикации: собирает кольцо, запускает DMA, дальше ведёт плавную
/// смену цифр и публикует состояние слотов (5 мс — шаг анимации, ~320 мс
/// на полный переход «старая цифра -> 0 -> новая цифра»).
#[embassy_executor::task]
pub async fn display_task(
    i2s: peripherals::I2S1<'static>,
    dma: peripherals::DMA_I2S1<'static>,
    pins: TxSixteenBits<'static>,
    ws: peripherals::GPIO3<'static>,
) -> ! {
    let mode = TEST_MODE.load(Ordering::Relaxed);
    let on = DISPLAY_ON.load(Ordering::Relaxed);
    let lamp = TEST_LAMP.load(Ordering::Relaxed);
    let samples = if on {
        wanted_samples(mode, clock::epoch_cached(), lamp_of(lamp))
    } else {
        [0; 3]
    };
    let mut fades = [display_core::LampFade::dark(); 6];
    for (f, c) in fades.iter_mut().zip(lamp_codes(samples)) {
        if on {
            f.snap(c);
        }
    }
    let initial_words = slot_words(&fades);
    let initial_flags = flags_now(!on, NO_STROBE.load(Ordering::Relaxed));
    // Публикуем ДО запуска DMA: первый же тик ISR не должен погасить кадр.
    publish(&initial_words, initial_flags);

    let descs_base = {
        // SAFETY: display_task — единственный пользователь RING; ссылка
        // заканчивается здесь, дальше кольцом владеют DMA и ISR.
        let ring = unsafe { &mut *core::ptr::addr_of_mut!(RING) };
        ring.build();
        if on {
            for slot in 0..3 {
                let a = &fades[slot];
                let b = &fades[slot + 3];
                ring.set_slot_pwm(slot, a.code(), a.duty(), b.code(), b.duty(), true);
            }
        }
        ring.descs.as_mut_ptr() as usize as u32
    };
    RING_BASE.store(descs_base, Ordering::Relaxed);

    log::info!(
        "display: one looping frame, {} words ({} us, ~{} Hz), {} desc ({} B ON chunks), PWM {} steps/lamp, TG on lanes 8..10",
        FRAME_WORDS,
        FRAME_WORDS as u64 * 787 / 1000,
        1_000_000_000u64 / (FRAME_WORDS as u64 * 787),
        DESC_COUNT,
        ON_BYTES,
        display_core::DUTY_MAX,
    );

    let start = {
        let ring = unsafe { &mut *core::ptr::addr_of_mut!(RING) };
        ring.start()
    };
    let par = I2sParallel::new(i2s, dma, Rate::from_hz(SAMPLE_HZ), pins, ws);

    let xfer = match par.send(start) {
        Ok(xfer) => xfer,
        Err((e, _par, _buf)) => {
            log::error!("display: circular DMA start failed: {:?}", e);
            loop {
                Timer::after_secs(1).await;
            }
        }
    };

    // OUT_EOF = конец хвостового dark-дескриптора, то есть граница кадра.
    // Порядок важен: `ChannelTx::new` погасил линию I2S1 на всех CPU, поэтому
    // сначала чистим possible pending, разрешаем источник, и только потом
    // `bind_handler` снова разрешает линию. `int_ena` esp-hal наружу не отдаёт.
    let regs = unsafe { &*esp32::I2S1::ptr() };
    regs.int_clr().write(|w| w.out_eof().clear_bit_by_one());
    regs.int_ena().modify(|_, w| w.out_eof().set_bit());
    bind_handler(
        Interrupt::I2S1,
        InterruptHandler::new(i2s1_eof_isr, Priority::Priority1),
    );
    log::info!("display: OUT_EOF armed (frame tick, ISR at Priority1)");

    hangdetect::beat(&hangdetect::FRAME);

    let mut last_on = on;
    let mut last_words = initial_words;
    let mut last_flags = initial_flags;
    let mut tick: u32 = 0;
    let mut logged = Instant::now();
    let mut logged_ticks = EOF_TICKS.load(Ordering::Relaxed);
    let mut logged_applied = APPLIED.load(Ordering::Relaxed);
    let mut logged_requests = REQUESTS.load(Ordering::Relaxed);
    loop {
        // 5 мс: шаг фейда. Секунда меняется с задержкой не больше одного тика,
        // а анимация успевает ~64 шага на переход.
        Timer::after_millis(5).await;
        hangdetect::beat(&hangdetect::FRAME);

        let epoch = clock::epoch_cached();
        let mode = TEST_MODE.load(Ordering::Relaxed);
        let on = DISPLAY_ON.load(Ordering::Relaxed);
        let lamp = TEST_LAMP.load(Ordering::Relaxed);
        let nostrobe = NO_STROBE.load(Ordering::Relaxed);
        let animate = FADE_ENABLED.load(Ordering::Relaxed);

        let wanted = if on {
            lamp_codes(wanted_samples(mode, epoch, lamp_of(lamp)))
        } else {
            [display_core::BLANK_CODE; 6]
        };
        if on && !last_on {
            // Включили индикацию — сразу полная яркость, без фейда из тьмы.
            for (f, c) in fades.iter_mut().zip(wanted) {
                f.snap(c);
            }
        }
        for (f, c) in fades.iter_mut().zip(wanted) {
            if on {
                f.step(c, animate);
            } else {
                f.blank();
            }
        }

        let words = slot_words(&fades);
        let flags = flags_now(!on, nostrobe);
        if words != last_words || flags != last_flags {
            // Лучше перезаписать заявку, чем показать смесь: ISR применит
            // её целиком на границе кадра.
            publish(&words, flags);
            REQUESTS.fetch_add(1, Ordering::Relaxed);
            last_words = words;
            last_flags = flags;
        }
        last_on = on;

        tick += 1;
        if tick % 400 == 0 {
            let now = Instant::now();
            let elapsed_us = (now - logged).as_micros().max(1);
            let ticks = EOF_TICKS.load(Ordering::Relaxed);
            let applied = APPLIED.load(Ordering::Relaxed);
            let requests = REQUESTS.load(Ordering::Relaxed);
            let addr = EOF_DESC.load(Ordering::Relaxed);
            let hz = ticks.wrapping_sub(logged_ticks) as u64 * 1_000_000 / elapsed_us;
            // desc должен быть ровно +27 (хвостовой дескриптор) каждый кадр.
            let desc = addr.wrapping_sub(RING_BASE.load(Ordering::Relaxed))
                / core::mem::size_of::<DmaDescriptor>() as u32;
            // int_raw/int_ena/state: если тик когда-нибудь «схлопнется», по этим
            // трём числам видно, виноват лишний источник прерывания (ena),
            // незакрытый pending (raw) или вставший FIFO (state).
            let raw = regs.int_raw().read().bits();
            let ena = regs.int_ena().read().bits();
            let state = regs.state().read().bits();
            log::info!(
                "display: alive, tx_idle={}, eof={} (~{} Hz), desc=+{}, applied={} (+{}), req={} (+{}), bad={}, mode={}, on={}, fade={}, post={}ok/{}to, sending={}, raw={:08x}, ena={:08x}, st={:08x}",
                xfer.is_done(),
                ticks,
                hz,
                desc,
                applied,
                applied.wrapping_sub(logged_applied),
                requests,
                requests.wrapping_sub(logged_requests),
                EOF_BAD_ADDR.load(Ordering::Relaxed),
                mode,
                on,
                animate,
                crate::logger::post_ok(),
                crate::logger::post_timeouts(),
                crate::logger::sending(),
                raw,
                ena,
                state,
            );
            if ticks == logged_ticks {
                // За целый интервал (2 с) ни одного EOF: DMA встал или ISR не
                // вызывается. Отдельной строкой, чтобы это не потерялось.
                log::error!(
                    "display: НЕТ ТИКА КАДРА (eof не менялся), int_raw={:08x}, int_ena={:08x}, state={:08x}",
                    raw,
                    ena,
                    state,
                );
            }
            logged = now;
            logged_ticks = ticks;
            logged_applied = applied;
            logged_requests = requests;
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
