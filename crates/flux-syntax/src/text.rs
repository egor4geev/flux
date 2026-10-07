//! Rope глазами tree-sitter: байтовые куски без копирования текста,
//! счёт переводов строк и символов.

use std::ops::Range;

use flux_core::Rope;

/// Текст с байта `byte` до конца его куска — для callback'а парсера.
/// Пустой срез означает конец текста.
pub(crate) fn chunk_from(text: &Rope, byte: usize) -> &[u8] {
    if byte >= text.len_bytes() {
        return &[];
    }
    // Кусок, *содержащий* байт: на стыке ropey отдаёт следующий кусок,
    // так что срез не пуст.
    let (chunk, chunk_start, _, _) = text.chunk_at_byte(byte);
    &chunk.as_bytes()[byte - chunk_start..]
}

/// Байты диапазона кусками rope — текст узла для предикатов запроса.
/// Диапазон обрезается по длине текста: узлы отредактированного, но ещё не
/// разобранного дерева могут выходить за конец.
pub(crate) fn byte_chunks(text: &Rope, range: Range<usize>) -> ByteChunks<'_> {
    let end = range.end.min(text.len_bytes());
    ByteChunks {
        text,
        pos: range.start.min(end),
        end,
    }
}

pub(crate) struct ByteChunks<'a> {
    text: &'a Rope,
    pos: usize,
    end: usize,
}

impl<'a> Iterator for ByteChunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.pos >= self.end {
            return None;
        }
        let (chunk, chunk_start, _, _) = self.text.chunk_at_byte(self.pos);
        let to = (self.end - chunk_start).min(chunk.len());
        let bytes = &chunk.as_bytes()[self.pos - chunk_start..to];
        self.pos += bytes.len();
        Some(bytes)
    }
}

/// Число `\n` в тексте. Строки tree-sitter делит только по ним.
pub(crate) fn count_newlines(text: &Rope) -> usize {
    text.chunks()
        .map(|chunk| count_byte(chunk.as_bytes(), b'\n'))
        .sum()
}

pub(crate) fn count_byte(bytes: &[u8], needle: u8) -> usize {
    bytes.iter().filter(|&&b| b == needle).count()
}

/// Переводит неубывающие байтовые смещения в число символов от начальной
/// точки — один проход по видимому тексту вместо `byte_to_char` (спуск по
/// дереву rope) на каждую границу спана.
pub(crate) struct CharCounter<'a> {
    text: &'a Rope,
    chunk: &'a [u8],
    chunk_start: usize,
    byte: usize,
    chars: usize,
}

impl<'a> CharCounter<'a> {
    pub fn new(text: &'a Rope, byte: usize) -> Self {
        let byte = byte.min(text.len_bytes());
        Self {
            text,
            chunk: &[],
            chunk_start: byte,
            byte,
            chars: 0,
        }
    }

    /// Сколько символов начинается между начальной точкой и `byte`.
    /// Смещение внутри многобайтового символа засчитывает этот символ:
    /// так получается граница символа, а не паника.
    pub fn chars_to(&mut self, byte: usize) -> usize {
        let byte = byte.min(self.text.len_bytes());
        while self.byte < byte {
            let offset = self.byte - self.chunk_start;
            if offset >= self.chunk.len() {
                let (chunk, chunk_start, _, _) = self.text.chunk_at_byte(self.byte);
                self.chunk = chunk.as_bytes();
                self.chunk_start = chunk_start;
                continue;
            }
            let to = (byte - self.chunk_start).min(self.chunk.len());
            self.chars += count_char_starts(&self.chunk[offset..to]);
            self.byte = self.chunk_start + to;
        }
        self.chars
    }
}

/// Первые байты символов UTF-8 — все, кроме продолжений `0b10xx_xxxx`.
fn count_char_starts(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| (b as i8) >= -0x40).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rope из многих кусков: правки посередине дробят его.
    fn chunky_rope() -> (Rope, String) {
        let mut text = Rope::new();
        let piece = "ab\nвг👍🏽\r\nx";
        for i in 0..400 {
            let at = (i * 7) % (text.len_chars() + 1);
            text.insert(at, piece);
        }
        let string = text.to_string();
        assert!(text.chunks().count() > 3);
        (text, string)
    }

    #[test]
    fn chunk_from_covers_whole_text() {
        let (text, string) = chunky_rope();
        let mut read = Vec::new();
        while read.len() < string.len() {
            let chunk = chunk_from(&text, read.len());
            assert!(!chunk.is_empty());
            read.extend_from_slice(chunk);
        }
        assert_eq!(read, string.as_bytes());
        assert!(chunk_from(&text, string.len()).is_empty());
        assert!(chunk_from(&text, string.len() + 10).is_empty());
    }

    #[test]
    fn byte_chunks_returns_exact_range() {
        let (text, string) = chunky_rope();
        let len = string.len();
        for (start, end) in [
            (0, len),
            (5, 900),
            (1000, 1001),
            (len - 3, len + 50),
            (len + 1, len + 2),
        ] {
            let got: Vec<u8> = byte_chunks(&text, start..end).flatten().copied().collect();
            let end = end.min(len);
            let start = start.min(end);
            assert_eq!(got, &string.as_bytes()[start..end]);
        }
    }

    #[test]
    fn counts_newlines_only() {
        let text = Rope::from_str("a\nb\r\nc\rd\u{2028}e\u{c}f\n");
        assert_eq!(count_newlines(&text), 3);
    }

    #[test]
    fn char_counter_matches_rope() {
        let (text, string) = chunky_rope();
        let mut counter = CharCounter::new(&text, 0);
        for (byte, _) in string.char_indices().step_by(13) {
            assert_eq!(counter.chars_to(byte), text.byte_to_char(byte));
        }
        assert_eq!(counter.chars_to(string.len() + 5), text.len_chars());
    }

    #[test]
    fn char_counter_rounds_up_inside_char() {
        let text = Rope::from_str("aж👍b");
        let mut counter = CharCounter::new(&text, 0);
        assert_eq!(counter.chars_to(2), 2, "middle of 'ж' counts it");
        assert_eq!(counter.chars_to(3), 2);
        assert_eq!(counter.chars_to(5), 3, "middle of the emoji");
        assert_eq!(counter.chars_to(7), 3);
        assert_eq!(counter.chars_to(8), 4);
    }
}
