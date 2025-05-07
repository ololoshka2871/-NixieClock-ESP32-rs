# NixieClock formware on Rust
Based on https://github.com/ImplFerris/esp32-projects/tree/main/webserver-html

## Схема
![](Schematic-lv.png)

| PIN | Цепь                  | Назначение                      |
| --- | --------------------- | ------------------------------- |
|     | *ADC*                 |                                 |
| 5   | 3.3v_sens             | Измерение напряжения 3,3 В      |
| 6   | 12v_sens              | Измерение напряжения 12 В       |
| 7   | 5v_sens               | Измерение напряжения 5 В        |
|     | *Monitor UART*        |                                 |
| 34  | RX                    | UART0 RXD                       |
| 35  | TX                    | UART0 TXD                       |
|     | *MH-Z19 UART*         |                                 |
| 27  | RX                    | UART2 RXD                       |
| 26  | TX                    | UART2 TXD                       |
|     | *Temperature sensor*  |                                 |
| 25  | 1-Wire                | 1-Wire data                     |
|     | *Buttons*             |                                 |
| 3   | RESET                 | Кнопка сброса                   |
| 25  | MODE                  | Кнопка режима                   |
|     | *Neopixel*            |                                 |
| 23  | LED_DATA              | Данные для адресных светодиодов |
|     | *RTC*                 |                                 |
| 33  | SDA                   | RTC I2C SDA                     |
| 36  | SCL                   | RTC I2C SCL                     |
| 37  | SQW                   | RTC импульс с периодом 1 с.     |
|     | *Nixie Tubes control* |                                 |
| 16  | TG0                   | Выбор лампы 0                   |
| 24  | TG1                   | Выбор лампы 1                   |
| 13  | TG2                   | Выбор лампы 2                   |
| 12  | D1A                   | Данные для группы ламп A бит 0  |
| 11  | D2A                   | Данные для группы ламп A бит 1  |
| 10  | D3A                   | Данные для группы ламп A бит 2  |
| 9   | D4A                   | Данные для группы ламп A бит 3  |
| 8   | D1B                   | Данные для группы ламп B бит 0  |
| 29  | D2B                   | Данные для группы ламп B бит 1  |
| 30  | D3B                   | Данные для группы ламп B бит 2  |
| 31  | D4B                   | Данные для группы ламп B бит 3  |
