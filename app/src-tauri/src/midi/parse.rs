//! Reads the MIDI messages a keyboard sends that Grooviq plays.

/// What a keyboard asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiMessage {
    /// A key went down; `velocity` is 1..=127.
    NoteOn {
        key: u8,
        velocity: u8,
    },
    NoteOff {
        key: u8,
    },
    /// The sustain pedal: `true` is down.
    Sustain(bool),
    /// Release every note, whatever holds it (all sound off, all notes off).
    AllNotesOff,
}

/// Reads one MIDI message; `None` for anything Grooviq does not play.
///
/// Any of the 16 MIDI channels plays. A note-on with velocity 0 is a note-off, a sustain
/// controller (64) from 64 up is the pedal down, and controllers 120 (all sound off) and 123
/// (all notes off) release everything.
pub fn parse(bytes: &[u8]) -> Option<MidiMessage> {
    // One channel message with two data bytes (a data byte has its high bit clear). Sysex, clock
    // and the one-data-byte messages (program change, channel pressure) are none of them.
    let &[status, first, second] = bytes else {
        return None;
    };
    if first > 0x7F || second > 0x7F {
        return None;
    }
    match status & 0xF0 {
        0x80 => Some(MidiMessage::NoteOff { key: first }),
        0x90 if second == 0 => Some(MidiMessage::NoteOff { key: first }),
        0x90 => Some(MidiMessage::NoteOn {
            key: first,
            velocity: second,
        }),
        0xB0 => match first {
            SUSTAIN => Some(MidiMessage::Sustain(second >= 64)),
            ALL_SOUND_OFF | ALL_NOTES_OFF => Some(MidiMessage::AllNotesOff),
            _ => None,
        },
        _ => None,
    }
}

const SUSTAIN: u8 = 64;
const ALL_SOUND_OFF: u8 = 120;
const ALL_NOTES_OFF: u8 = 123;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_notes_on_any_channel_and_velocity_zero_as_off() {
        for channel in 0..16u8 {
            assert_eq!(
                parse(&[0x90 | channel, 60, 100]),
                Some(MidiMessage::NoteOn {
                    key: 60,
                    velocity: 100
                }),
                "note on, channel {channel}"
            );
            assert_eq!(
                parse(&[0x80 | channel, 61, 40]),
                Some(MidiMessage::NoteOff { key: 61 }),
                "note off, channel {channel}"
            );
            // A note-on with velocity 0 is how many keyboards send a release.
            assert_eq!(
                parse(&[0x90 | channel, 62, 0]),
                Some(MidiMessage::NoteOff { key: 62 }),
                "velocity 0, channel {channel}"
            );
        }
        assert_eq!(
            parse(&[0x90, 127, 1]),
            Some(MidiMessage::NoteOn {
                key: 127,
                velocity: 1
            })
        );
    }

    #[test]
    fn the_pedal_is_down_from_64_and_two_controllers_release_everything() {
        assert_eq!(parse(&[0xB0, 64, 127]), Some(MidiMessage::Sustain(true)));
        assert_eq!(parse(&[0xB3, 64, 64]), Some(MidiMessage::Sustain(true)));
        assert_eq!(parse(&[0xB0, 64, 63]), Some(MidiMessage::Sustain(false)));
        assert_eq!(parse(&[0xB0, 64, 0]), Some(MidiMessage::Sustain(false)));
        // All sound off and all notes off.
        assert_eq!(parse(&[0xB0, 120, 0]), Some(MidiMessage::AllNotesOff));
        assert_eq!(parse(&[0xBF, 123, 0]), Some(MidiMessage::AllNotesOff));
    }

    #[test]
    fn everything_else_is_ignored() {
        let ignored: [&[u8]; 11] = [
            &[],
            &[0x90],
            &[0x90, 60],
            // Other controllers, pitch bend, aftertouch, program change, sysex, clock.
            &[0xB0, 1, 99],
            &[0xB0, 121, 0],
            &[0xE0, 0, 64],
            &[0xA0, 60, 30],
            &[0xC0, 5],
            &[0xF0, 0x7E, 0x7F, 0xF7],
            &[0xF8],
            // A data byte with the high bit set is not a message.
            &[0x90, 60, 200],
        ];
        for bytes in ignored {
            assert_eq!(parse(bytes), None, "{bytes:02x?}");
        }
        // More bytes than a channel message holds is not one either.
        assert_eq!(parse(&[0x90, 60, 100, 0x90, 62, 100]), None);
    }
}
