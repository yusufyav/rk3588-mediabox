//! Türksat Kanal Güncelleme Sistemi (TKGS), read from PID 0x1FF5's private
//! long table. This is an independent implementation of the published wire
//! facts and ETSI descriptors.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ts::{Sections, Service, dvb_text, parse_sdt, payload_of, pid_of};

pub const PID_TKGS: u16 = 0x1FF5;
pub const TABLE_TKGS: u8 = 0xA7;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TkgsDelivery {
    pub frequency_khz: u32,
    pub symbol_rate_ksps: u32,
    pub orbital_position_tenths: u16,
    pub east: bool,
    pub polarisation: String,
    pub roll_off: String,
    pub dvb_s2: bool,
    pub modulation: String,
    pub fec_inner: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TkgsService {
    pub service_id: u16,
    pub lcn: u16,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_type: Option<u8>,
    pub free_ca_mode: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TkgsTransponder {
    pub transport_stream_id: u16,
    pub original_network_id: u16,
    pub delivery: TkgsDelivery,
    pub services: Vec<TkgsService>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TkgsOrdering {
    pub name: String,
    pub lcns: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TkgsCategory {
    pub id: u8,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TkgsTable {
    pub table_id_extension: u16,
    pub version: u8,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub satellite: Option<String>,
    pub transponders: Vec<TkgsTransponder>,
    pub orderings: Vec<TkgsOrdering>,
    pub categories: Vec<TkgsCategory>,
}

impl TkgsTable {
    pub fn channel_count(&self) -> usize {
        self.transponders.iter().map(|t| t.services.len()).sum()
    }
}

#[derive(Default)]
pub struct TkgsCollector {
    sections: Sections,
    extension: Option<u16>,
    version: Option<u8>,
    last_section: Option<u8>,
    parts: BTreeMap<u8, Vec<u8>>,
    table: Option<TkgsTable>,
    error: Option<String>,
}

impl TkgsCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one already aligned MPEG-TS packet.
    pub fn packet(&mut self, packet: &[u8]) {
        if packet.len() != crate::ts::PACKET
            || pid_of(packet) != PID_TKGS
            || packet[1] & 0x80 != 0
            || packet[3] >> 6 != 0
        {
            return;
        }
        let Some(payload) = payload_of(packet) else {
            return;
        };
        let mut sections = std::mem::take(&mut self.sections);
        sections.payload(PID_TKGS, packet[1] & 0x40 != 0, payload, |section| {
            self.section(section)
        });
        self.sections = sections;
    }

    fn section(&mut self, section: &[u8]) {
        if section.len() < 12 || section[0] != TABLE_TKGS || section[5] & 1 == 0 {
            return;
        }
        let extension = u16::from_be_bytes([section[3], section[4]]);
        let version = (section[5] >> 1) & 0x1F;
        let number = section[6];
        let last = section[7];
        if number > last {
            return;
        }
        if self.extension != Some(extension)
            || self.version != Some(version)
            || self.last_section != Some(last)
        {
            self.extension = Some(extension);
            self.version = Some(version);
            self.last_section = Some(last);
            self.parts.clear();
            self.table = None;
            self.error = None;
        }
        self.parts
            .insert(number, section[8..section.len() - 4].to_vec());
        if self.parts.len() != last as usize + 1 || !(0..=last).all(|n| self.parts.contains_key(&n))
        {
            return;
        }
        let mut block = Vec::new();
        for number in 0..=last {
            block.extend_from_slice(&self.parts[&number]);
        }
        match parse_block(extension, version, &block) {
            Ok(table) => self.table = Some(table),
            Err(error) => self.error = Some(error),
        }
    }

    pub fn table(&self) -> Option<&TkgsTable> {
        self.table.as_ref()
    }

    pub fn take(&mut self) -> Option<TkgsTable> {
        self.table.take()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn progress(&self) -> (usize, usize) {
        (
            self.parts.len(),
            self.last_section.map_or(0, |last| last as usize + 1),
        )
    }
}

fn be16(data: &[u8], at: usize) -> Result<u16, String> {
    let bytes = data
        .get(at..at + 2)
        .ok_or_else(|| format!("TKGS block ended at byte {at}"))?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn descriptor_loop(data: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut at = 0;
    std::iter::from_fn(move || {
        let tag = *data.get(at)?;
        let len = *data.get(at + 1)? as usize;
        let body = data.get(at + 2..at + 2 + len)?;
        at += 2 + len;
        Some((tag, body))
    })
}

fn bcd(data: &[u8], nibbles: usize) -> Option<u32> {
    let mut value = 0u32;
    for i in 0..nibbles {
        let byte = *data.get(i / 2)?;
        let digit = if i % 2 == 0 { byte >> 4 } else { byte & 0x0F };
        if digit > 9 {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(digit as u32)?;
    }
    Some(value)
}

fn delivery(d: &[u8]) -> Result<TkgsDelivery, String> {
    if d.len() < 11 {
        return Err("TKGS satellite_delivery_system_descriptor is shorter than 11 bytes".into());
    }
    let frequency_khz = bcd(&d[..4], 8)
        .ok_or("invalid BCD frequency")?
        .checked_mul(10)
        .ok_or("frequency overflow")?;
    let orbital_position_tenths = bcd(&d[4..6], 4).ok_or("invalid BCD orbital position")? as u16;
    let flags = d[6];
    let polarisation =
        ["horizontal", "vertical", "left", "right"][(flags >> 5 & 3) as usize].to_string();
    let roll_off = ["0.35", "0.25", "0.20", "reserved"][(flags >> 3 & 3) as usize].to_string();
    let dvb_s2 = flags & 0x04 != 0;
    let modulation = ["auto", "qpsk", "8psk", "16-qam"][(flags & 3) as usize].to_string();
    let symbol_rate_ksps = bcd(&d[7..11], 7).ok_or("invalid BCD symbol rate")? / 10;
    let fec_inner = match d[10] & 0x0F {
        0 => "undefined",
        1 => "1/2",
        2 => "2/3",
        3 => "3/4",
        4 => "5/6",
        5 => "7/8",
        6 => "8/9",
        7 => "3/5",
        8 => "4/5",
        9 => "9/10",
        15 => "none",
        _ => "reserved",
    }
    .to_string();
    Ok(TkgsDelivery {
        frequency_khz,
        symbol_rate_ksps,
        orbital_position_tenths,
        east: flags & 0x80 != 0,
        polarisation,
        roll_off,
        dvb_s2,
        modulation,
        fec_inner,
    })
}

fn service_categories(section: &[u8]) -> BTreeMap<u16, Vec<u8>> {
    let mut out = BTreeMap::new();
    if section.len() < 15 {
        return out;
    }
    let body = &section[8..section.len().saturating_sub(4)];
    let mut at = 3;
    while at + 5 <= body.len() {
        let sid = u16::from_be_bytes([body[at], body[at + 1]]);
        let len = (((body[at + 3] & 0x0F) as usize) << 8) | body[at + 4] as usize;
        let end = (at + 5 + len).min(body.len());
        let found = descriptor_loop(&body[at + 5..end])
            .filter(|(tag, _)| *tag == 0x95)
            .flat_map(|(_, d)| d.iter().copied())
            .collect::<Vec<_>>();
        if !found.is_empty() {
            out.insert(sid, found);
        }
        at += 5 + len;
    }
    out
}

fn parse_lcns(data: &[u8]) -> Result<BTreeMap<u16, u16>, String> {
    let mut out = BTreeMap::new();
    let mut at = 0;
    while at < data.len() {
        if at + 7 > data.len() {
            return Err(format!("TKGS LCN record truncated at byte {at}"));
        }
        let extra = data[at + 4] as usize;
        let size = 5usize
            .checked_add(extra)
            .ok_or("TKGS LCN record size overflow")?;
        if extra < 2 || at + size > data.len() {
            return Err(format!("invalid TKGS LCN record size {size} at byte {at}"));
        }
        let lcn = u16::from_be_bytes([data[at], data[at + 1]]);
        let sid = u16::from_be_bytes([data[at + 5], data[at + 6]]);
        out.insert(sid, lcn);
        at += size;
    }
    Ok(out)
}

fn labelled_text(data: &[u8]) -> String {
    let Some(&len) = data.get(3) else { return String::new() };
    let text = data.get(4..4 + len as usize).unwrap_or_default();
    if data.get(..3) == Some(b"TUR") {
        let mut encoded = Vec::with_capacity(text.len() + 1);
        encoded.push(0x05);
        encoded.extend_from_slice(text);
        dvb_text(&encoded)
    } else {
        dvb_text(text)
    }
}

fn tkgs_turkish(text: String) -> String {
    text.chars()
        .map(|c| match c {
            'Ð' => 'Ğ',
            'Ý' => 'İ',
            'Þ' => 'Ş',
            'ð' => 'ğ',
            'ý' => 'ı',
            'þ' => 'ş',
            other => other,
        })
        .collect()
}

fn parse_block(extension: u16, version: u8, data: &[u8]) -> Result<TkgsTable, String> {
    let header_len = be16(data, 0)? as usize;
    let header = data
        .get(2..2 + header_len)
        .ok_or("TKGS header descriptor loop is truncated")?;
    let title = descriptor_loop(header)
        .find(|(tag, _)| *tag == 0x90)
        .map(|(_, d)| dvb_text(d))
        .unwrap_or_default();
    let mut at = 2 + header_len;
    // Present in the real 2026-08-19 table, but not named by the available
    // wire facts. Keep its boundary without assigning it a meaning.
    data.get(at..at + 3)
        .ok_or("TKGS pre-satellite field is truncated")?;
    at += 3;
    let mut satellite = None;
    if data.get(at + 1) == Some(&0xA0) {
        let block_len = *data.get(at).ok_or("TKGS satellite block is truncated")? as usize;
        let block = data
            .get(at + 1..at + 1 + block_len)
            .ok_or("TKGS satellite block body is truncated")?;
        satellite = descriptor_loop(block)
            .find(|(tag, _)| *tag == 0xA0)
            .map(|(_, body)| dvb_text(body));
        at += 1 + block_len;
    }
    data.get(at..at + 3)
        .ok_or("TKGS pre-transponder field is truncated")?;
    at += 3;

    let mut transponders = Vec::new();
    while data
        .get(at)
        .copied()
        .ok_or("TKGS transponder terminator is missing")?
        != 0
    {
        let desc_len = data[at] as usize;
        at += 1;
        let desc = data
            .get(at..at + desc_len)
            .ok_or("TKGS transponder descriptor loop is truncated")?;
        at += desc_len;
        let delivery = descriptor_loop(desc)
            .find(|(tag, _)| *tag == 0x43)
            .ok_or_else(|| "TKGS transponder has no satellite delivery descriptor".to_string())
            .and_then(|(_, d)| delivery(d))?;
        let mut services = BTreeMap::<u16, Service>::new();
        let mut service_cats = BTreeMap::<u16, Vec<u8>>::new();
        let mut tsid = 0;
        let mut onid = 0;
        while data.get(at) == Some(&0x42) {
            let section_len = (((*data.get(at + 1).ok_or("embedded SDT header truncated")? & 0x0F)
                as usize)
                << 8)
                | *data.get(at + 2).ok_or("embedded SDT header truncated")? as usize;
            let section = data
                .get(at..at + 3 + section_len)
                .ok_or("embedded SDT section truncated")?;
            // The enclosing TKGS sections have already passed CRC validation.
            // Some broadcast SDT copies carry stale inner CRCs, so their
            // declared boundary is authoritative here.
            if section.len() < 12 {
                return Err(format!("transponder {} embedded SDT at {at:#x} is too short", transponders.len()));
            }
            tsid = u16::from_be_bytes([section[3], section[4]]);
            let body = &section[8..section.len() - 4];
            if body.len() >= 2 {
                onid = u16::from_be_bytes([body[0], body[1]]);
            }
            parse_sdt(body, &mut services);
            service_cats.extend(service_categories(section));
            at += section.len();
        }
        let lcn_len = be16(data, at)? as usize;
        at += 2;
        let lcns = parse_lcns(
            data.get(at..at + lcn_len)
                .ok_or("TKGS LCN block is truncated")?,
        )
        .map_err(|error| format!("transponder {} LCN block at {at:#x}: {error}", transponders.len()))?;
        at += lcn_len;
        let services = lcns
            .into_iter()
            .map(|(service_id, lcn)| {
                let service = services.get(&service_id).cloned().unwrap_or_default();
                TkgsService {
                    service_id,
                    lcn,
                    name: tkgs_turkish(service.name.unwrap_or_default()),
                    provider: service.provider.filter(|p| !p.is_empty()).map(tkgs_turkish),
                    service_type: service.service_type,
                    free_ca_mode: service.free_ca_mode,
                    categories: service_cats.remove(&service_id).unwrap_or_default(),
                }
            })
            .collect();
        transponders.push(TkgsTransponder {
            transport_stream_id: tsid,
            original_network_id: onid,
            delivery,
            services,
        });
    }
    at += 1;

    let mut orderings = Vec::new();
    let mut categories = Vec::new();
    let ordering_len = be16(data, at)? as usize;
    at += 2;
    let ordering_end = at.checked_add(ordering_len).ok_or("TKGS ordering block size overflow")?;
    if ordering_end > data.len() {
        return Err("TKGS ordering block is truncated".into());
    }
    while at + 2 <= ordering_end {
        let block_len = be16(data, at)? as usize;
        at += 2;
        let block = data.get(at..at + block_len).filter(|_| at + block_len <= ordering_end)
            .ok_or("TKGS ordering descriptor block is truncated")?;
        at += block_len;
        let mut ordering_name = String::new();
        let mut ordering_lcns = Vec::new();
        for (tag, body) in descriptor_loop(block) {
            match tag {
                0x93 => ordering_name = labelled_text(body),
                // Each record maps a display position to a channel LCN.
                0x94 if body.len() >= 4 => ordering_lcns.push(u16::from_be_bytes([body[2], body[3]])),
                0x92 if body.len() >= 6 => categories.push(TkgsCategory {
                    id: body[1],
                    name: labelled_text(&body[2..]),
                }),
                _ => {}
            }
        }
        if !ordering_name.is_empty() {
            orderings.push(TkgsOrdering { name: ordering_name, lcns: ordering_lcns });
        }
    }
    at = ordering_end;
    let category_len = be16(data, at)? as usize;
    at += 2;
    let category_block = data.get(at..at + category_len).ok_or("TKGS category descriptor block is truncated")?;
    for (tag, body) in descriptor_loop(category_block) {
        if tag == 0x92 && body.len() >= 6 {
            categories.push(TkgsCategory { id: body[1], name: labelled_text(&body[2..]) });
        }
    }
    let known: BTreeSet<u16> = transponders
        .iter()
        .flat_map(|t| t.services.iter().map(|s| s.lcn))
        .collect();
    for ordering in &mut orderings {
        ordering.lcns.retain(|lcn| known.contains(lcn));
    }
    Ok(TkgsTable {
        table_id_extension: extension,
        version,
        title,
        satellite,
        transponders,
        orderings,
        categories,
    })
}
