use clap::{App, Arg};
use keri::{
    derivation::{basic::Basic, self_addressing::SelfAddressing, self_signing::SelfSigning},
    error::Error,
    event::{
        event_data::{
            inception::InceptionEvent, interaction::InteractionEvent, receipt::ReceiptTransferable,
            rotation::RotationEvent, EventData,
        },
        sections::{seal::EventSeal, InceptionWitnessConfig, KeyConfig, WitnessConfig},
        Event,
    },
    event_message::{
        parse::signed_message, serialization_info::SerializationFormats, EventMessage,
        SignedEventMessage,
    },
    prefix::{AttachedSignaturePrefix, IdentifierPrefix, Prefix, SelfAddressingPrefix},
    state::IdentifierState,
    util::dfs_serializer,
};
use std::{
    collections::HashMap,
    io::prelude::*,
    net::{SocketAddr, TcpListener, TcpStream},
    str::from_utf8_unchecked,
};
use ursa::{
    keys::{PrivateKey, PublicKey},
    signatures::{ed25519, SignatureScheme},
};

struct LogState {
    pub log: Vec<SignedEventMessage>,
    pub sigs_map: HashMap<u64, Vec<SignedEventMessage>>,
    pub state: IdentifierState,
    pub keypair: (PublicKey, PrivateKey),
    pub next_keypair: (PublicKey, PrivateKey),
    pub escrow_sigs: Vec<SignedEventMessage>,
}
impl LogState {
    // incept a state and keys
    fn new() -> Result<LogState, Error> {
        let ed = ed25519::Ed25519Sha512::new();
        let keypair = ed
            .keypair(Option::None)
            .map_err(|e| Error::CryptoError(e))?;
        let next_keypair = ed
            .keypair(Option::None)
            .map_err(|e| Error::CryptoError(e))?;

        let icp_data = InceptionEvent {
            key_config: KeyConfig {
                threshold: 1,
                public_keys: vec![Basic::Ed25519.derive(keypair.0.clone())],
                threshold_key_digest: SelfAddressing::Blake3_256.derive(
                    Basic::Ed25519
                        .derive(next_keypair.0.clone())
                        .to_str()
                        .as_bytes(),
                ),
            },
            witness_config: InceptionWitnessConfig::default(),
            inception_configuration: vec![],
        };

        let icp_data_message = EventMessage::get_inception_data(
            &icp_data,
            SelfAddressing::Blake3_256,
            &SerializationFormats::JSON,
        );

        let pref = IdentifierPrefix::SelfAddressing(
            SelfAddressing::Blake3_256.derive(&dfs_serializer::to_vec(&icp_data_message)?),
        );

        let icp_m = Event {
            prefix: pref.clone(),
            sn: 0,
            event_data: EventData::Icp(icp_data),
        }
        .to_message(&SerializationFormats::JSON)?;

        let sigged = icp_m.sign(vec![AttachedSignaturePrefix::new(
            SelfSigning::Ed25519Sha512,
            ed.sign(&icp_m.serialize()?, &keypair.1)
                .map_err(|e| Error::CryptoError(e))?,
            0,
        )]);

        let s0 = IdentifierState::default().verify_and_apply(&sigged)?;

        Ok(LogState {
            log: vec![sigged],
            sigs_map: HashMap::new(),
            state: s0,
            keypair,
            next_keypair,
            escrow_sigs: vec![],
        })
    }

    // take a receipt made by validator, verify it and add to sigs_map or escrow
    fn add_sig(
        &mut self,
        validator: &IdentifierState,
        sigs: SignedEventMessage,
    ) -> Result<(), Error> {
        match sigs.event_message.event.event_data.clone() {
            EventData::Vrc(rct) => {
                let event = self
                    .log
                    .get(sigs.event_message.event.sn as usize)
                    .ok_or(Error::SemanticError("incorrect receipt sn".into()))?;

                // This logic can in future be moved to the correct place in the Kever equivalent here
                // receipt pref is the ID who made the event being receipted
                if sigs.event_message.event.prefix == self.state.prefix
                            // dig is the digest of the event being receipted
                            && rct.receipted_event_digest
                                == rct
                                    .receipted_event_digest
                                    .derivation
                                    .derive(&event.event_message.serialize()?)
                            // seal pref is the pref of the validator
                            && rct.validator_location_seal.prefix == validator.prefix
                {
                    if rct.validator_location_seal.event_digest
                        == rct
                            .validator_location_seal
                            .event_digest
                            .derivation
                            .derive(&validator.last)
                    {
                        // seal dig is the digest of the last establishment event for the validator, verify the rct
                        validator.verify(&event.event_message.sign(sigs.signatures.clone()))?;
                        self.sigs_map
                            .entry(sigs.event_message.event.sn)
                            .or_insert_with(|| vec![])
                            .push(sigs);
                    } else {
                        // escrow the seal
                        self.escrow_sigs.push(sigs)
                    }
                    Ok(())
                } else {
                    Err(Error::SemanticError("incorrect receipt binding".into()))
                }
            }
            _ => Err(Error::SemanticError("not a receipt".into())),
        }
    }

    fn make_rct(&self, event: EventMessage) -> Result<SignedEventMessage, Error> {
        let ser = event.serialize()?;
        Ok(Event {
            prefix: event.event.prefix,
            sn: event.event.sn,
            event_data: EventData::Vrc(ReceiptTransferable {
                receipted_event_digest: SelfAddressing::Blake3_256.derive(&ser),
                validator_location_seal: EventSeal {
                    prefix: self.state.prefix.clone(),
                    event_digest: SelfAddressing::Blake3_256.derive(&self.state.last),
                },
            }),
        }
        .to_message(&SerializationFormats::JSON)?
        .sign(vec![AttachedSignaturePrefix::new(
            SelfSigning::Ed25519Sha512,
            ed25519::Ed25519Sha512::new()
                .sign(&ser, &self.keypair.1)
                .map_err(|e| Error::CryptoError(e))?,
            0,
        )]))
    }

    fn rotate(&mut self) -> Result<SignedEventMessage, Error> {
        let ed = ed25519::Ed25519Sha512::new();
        let keypair = self.next_keypair.clone();
        let next_keypair = ed
            .keypair(Option::None)
            .map_err(|e| Error::CryptoError(e))?;

        let ev = Event {
            prefix: self.state.prefix.clone(),
            sn: self.state.sn + 1,
            event_data: EventData::Rot(RotationEvent {
                previous_event_hash: SelfAddressing::Blake3_256.derive(&self.state.last),
                key_config: KeyConfig {
                    threshold: 1,
                    public_keys: vec![Basic::Ed25519.derive(keypair.0.clone())],
                    threshold_key_digest: SelfAddressing::Blake3_256.derive(
                        Basic::Ed25519
                            .derive(next_keypair.0.clone())
                            .to_str()
                            .as_bytes(),
                    ),
                },
                witness_config: WitnessConfig::default(),
                data: vec![],
            }),
        }
        .to_message(&SerializationFormats::JSON)?;

        let rot = ev.sign(vec![AttachedSignaturePrefix::new(
            SelfSigning::Ed25519Sha512,
            ed25519::Ed25519Sha512::new()
                .sign(&ev.serialize()?, &keypair.1)
                .map_err(|e| Error::CryptoError(e))?,
            0,
        )]);

        self.state = self.state.clone().verify_and_apply(&rot)?;

        self.log.push(rot.clone());

        self.keypair = keypair;
        self.next_keypair = next_keypair;

        Ok(rot)
    }
}

fn main() -> ! {
    let matches = App::new("KERI Direct Mode TCP demo")
        .version("0.1")
        .author("Jolocom & Human Colossus Foundation")
        .arg(
            Arg::with_name("PORT")
                .help("Which port to use, format <hostname>:<port>, e.g. 127.0.0.1:443 or localhost:123")
                .required(true)
                .index(1),
        )
        .arg(
            Arg::with_name("connect")
                .help("connects to given port instead of listening")
                .short("c")
                .long("connect")
                .takes_value(false),
        )
        .get_matches();

    // use string of format <hostname>:<port>
    let port: SocketAddr = matches
        .value_of("PORT")
        .unwrap()
        .parse()
        .expect("not a parsable port value");
    let connect = matches.is_present("connect");

    let mut i = LogState::new().unwrap();
    let mut they = IdentifierState::default();
    let mut buf = [0u8; 2048];

    let mut stream = if connect {
        // connect to PORT
        TcpStream::connect(port).unwrap()
    } else {
        // listen on PORT
        let listener = TcpListener::bind(port).unwrap();
        listener.accept().unwrap().0
    };

    stream
        .write(&i.log.last().unwrap().serialize().unwrap())
        .unwrap();
    println!("------\ninitial local state: {:?}\n", i.state);
    println!("sent:\n{}", unsafe { from_utf8_unchecked(&i.state.last) });

    loop {
        // read incoming
        stream.read(&mut buf).unwrap();

        // TODO HACK NOT GOOD but otherwise the parsing functions need to be refactored to take &[u8]
        let ev = signed_message(unsafe { from_utf8_unchecked(&buf) })
            .unwrap()
            .1;

        match ev.event_message.event.event_data {
            EventData::Vrc(_) => {
                i.add_sig(&they, ev).unwrap();
                i.rotate().unwrap();
                println!("------\nnew local state: {:?}\n", i.state);
                stream.write(&i.state.last).unwrap();
            }
            _ => {
                they = they.verify_and_apply(&ev).unwrap();
                println!("------\nnew remote state: {:?}\n", they);
            }
        }
    }
}
