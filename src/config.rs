pub(crate) const SSID: &str = "NixieClock";
pub(crate) const PASSWORD: &str = "ClockPassword937";

// Unlike Station mode, You can give any IP range(private) that you like
// IP Address/Subnet mask eg: STATIC_IP=192.168.13.37/24
pub(crate) const STATIC_IP: embassy_net::Ipv4Cidr =
    embassy_net::Ipv4Cidr::new(esp_hal_dhcp_server::Ipv4Addr::new(192, 168, 13, 37), 24);

//-----------------------------------------------------------------------------

pub const MHZ_19_UART_BUTRATE: u32 = 9600;

//-----------------------------------------------------------------------------

pub const V12_DEVIDER: (f32, f32) = (24000.0, 1000.0); 
pub const V5_DEVIDER: (f32, f32) = (9100.0, 1000.0); 
pub const V3V3_DEVIDER: (f32, f32) = (4700.0, 1000.0); 
