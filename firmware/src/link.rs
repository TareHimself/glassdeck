//! The serial link to the keyboard's computer: UART1 at 921600 8N1, HDLC frames. Transmit
//! blocks until the bytes are in the FIFO, as the stock firmware's `uart_write_blocking` does.

use glassdeck_scanner_proto::hdlc::{self, Decoder, SERIAL_MAX_PAYLOAD};
use glassdeck_scanner_proto::serial::{RESPONSE_LEN, Response, SENSOR_READINGS_LEN};
use rp2040_hal::pac;

const WIRE_LEN: usize = hdlc::max_encoded_len(SENSOR_READINGS_LEN);

fn uart() -> &'static pac::uart0::RegisterBlock {
    // SAFETY: UART1 is the link's alone, set up by the HAL before the loop.
    unsafe { &*pac::UART1::ptr() }
}

pub struct Link {
    decoder: Decoder<SERIAL_MAX_PAYLOAD>,
    wire: [u8; WIRE_LEN],
}

impl Link {
    pub fn new() -> Self {
        Self {
            decoder: Decoder::default(),
            wire: [0; WIRE_LEN],
        }
    }

    pub fn send(&mut self, payload: &[u8]) {
        let len = hdlc::encode::<SERIAL_MAX_PAYLOAD>(payload, &mut self.wire)
            .expect("scanner messages fit the link");
        for &b in &self.wire[..len] {
            while uart().uartfr().read().txff().bit_is_set() {}
            uart().uartdr().write(|w| unsafe { w.data().bits(b) });
        }
    }

    /// Reads every byte waiting, calling `on_response` for each valid response. Like the stock
    /// firmware, frames that aren't a 4-byte response to a key-state or sensor message are dropped.
    pub fn receive(&mut self, mut on_response: impl FnMut(&Response)) {
        while uart().uartfr().read().rxfe().bit_is_clear() {
            let b = uart().uartdr().read().data().bits();
            if let Some(Ok(payload)) = self.decoder.push(b)
                && payload.len() == RESPONSE_LEN
                && let Ok(response) = Response::parse(payload)
            {
                on_response(&response);
            }
        }
    }

    /// Drops everything received so far, after resetting the keyboard's computer.
    pub fn discard_input(&mut self) {
        while uart().uartfr().read().rxfe().bit_is_clear() {
            let _ = uart().uartdr().read();
        }
        self.decoder = Decoder::default();
    }
}
