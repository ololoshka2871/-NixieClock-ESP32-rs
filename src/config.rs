pub(crate) const SSID: &str = "NixieClock";
pub(crate) const PASSWORD: &str = "ClockPassword937";

// Unlike Station mode, You can give any IP range(private) that you like
// IP Address/Subnet mask eg: STATIC_IP=192.168.13.37/24
pub(crate) const STATIC_IP: embassy_net::Ipv4Cidr =
    embassy_net::Ipv4Cidr::new(esp_hal_dhcp_server::Ipv4Addr::new(192, 168, 13, 37), 24);

//-----------------------------------------------------------------------------

pub const MHZ_19_UART_BUTRATE: u32 = 9600;
