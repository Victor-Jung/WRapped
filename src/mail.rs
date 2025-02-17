extern crate imap;
extern crate native_tls;
extern crate chrono;

use itertools::join;
use native_tls::TlsConnector;
use chrono::{DateTime, FixedOffset};
use std::str::from_utf8;

use crate::config::{MailConfig, MailLogin, MailFetch};

#[derive(Debug, Clone)]
pub struct Address {
    pub name: Option<String>,
    pub user: Option<String>,
    pub email: Option<String>
}

impl Address {
    pub fn from_imap_address(addr: &imap_proto::types::Address) -> Self {
        Address {
            name: addr.name.as_ref().map(|s| String::from_utf8_lossy(s).to_string()),
            user: addr.mailbox.as_ref().map(|s| String::from_utf8_lossy(s).to_string()),
            email: {
                let host = addr.host.as_ref().map(|s| String::from_utf8_lossy(s).to_string());
                let mailbox = addr.mailbox.as_ref().map(|s| String::from_utf8_lossy(s).to_string());
                match (mailbox, host) {
                    (Some(mailbox), Some(host)) => Some(format!("{}@{}", mailbox, host)),
                    _ => None,
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Envelope {
    pub date: DateTime<FixedOffset>,
    pub subject: String,
    pub cc: Option<Vec<Address>>,
    pub in_reply_to: Option<String>,
    pub message_id: Option<String>,
}

impl Envelope {
    pub fn from_imap_envelope(envelope: &imap_proto::types::Envelope) -> Self {
        Envelope {
            date: {
                let date_str = envelope.date.as_ref().map(|s| String::from_utf8_lossy(s).to_string());
                DateTime::parse_from_rfc2822(&date_str.unwrap()).unwrap()
            },
            subject: String::from_utf8_lossy(envelope.subject.as_ref().unwrap()).to_string(),
            cc: envelope.cc.as_ref().map(|cc| cc.iter().map(|addr| Address::from_imap_address(addr)).collect()),
            in_reply_to: envelope.in_reply_to.as_ref().map(|s| String::from_utf8_lossy(s).to_string()),
            message_id: envelope.message_id.as_ref().map(|s| String::from_utf8_lossy(s).to_string()),
        }
    }
}

fn imap_login(
    login: &MailLogin,
) -> imap::error::Result<imap::Session<native_tls::TlsStream<std::net::TcpStream>>> {

    let domain = login.server.as_str();
    let port = login.port;
    let username = login.username.as_str();
    let password = login.password.as_str();

    // Connect to the IMAP server
    let tls = TlsConnector::builder().build().unwrap();
    let client = imap::connect((domain, port), domain, &tls).unwrap();

    // Login to the IMAP server
    let imap_session = client
        .login(username, password)
        .map_err(|e| e.0)?;

    Ok(imap_session)
}

pub fn list_mailboxes(
    config: &MailConfig,
) -> imap::error::Result<()> {

    // Login to the IMAP server
    let mut imap_session = imap_login(&config.login)?;

    // List all mailboxes
    let mailboxes = imap_session.list(Some(""), Some("*"))?;
    println!("Mailboxes:");
    for mailbox in mailboxes.iter() {
        println!("{}", mailbox.name());
    }

    // Logout from the IMAP server
    imap_session.logout()?;
    Ok(())
}

pub fn fetch_inbox(
    config: &MailConfig,
) -> imap::error::Result<()> {

    // Login to the IMAP server
    let mut imap_session = imap_login(&config.login)?;

    // Select the INBOX mailbox
    imap_session.select("INBOX")?;

    // Fetch the first message (only the ENVELOPE)
    let messages = imap_session.fetch("1", "ENVELOPE")?;
    let message = if let Some(message) = messages.iter().next() {
        message
    } else {
        return Ok(());
    };

    // Print the subject of the message
    let envelope = message.envelope().unwrap();
    let subject = envelope.subject.unwrap();
    let subject_str = std::str::from_utf8(subject).unwrap();
    println!("Got Mail with Subject: {}", subject_str);

    // Logout from the IMAP server
    imap_session.logout()?;
    Ok(())
}

fn build_imap_search_query(
    fetch: &MailFetch
) -> Result <String, String> {

    // Check that patterns is not empty
    if fetch.pattern.is_empty() {
        return Err("IMAP search query needs at least one pattern".to_string());
    }

    // Check that patterns has at most two elements
    if fetch.pattern.len() > 2 {
        return Err("IMAP search query supports a maximum of two patterns".to_string());
    }

    // Format the subject of the query
    let mut query = match fetch.pattern.len() {
        1 => format!("SUBJECT \"{}\"", fetch.pattern[0]),
        2 => format!("SUBJECT \"{}\" OR SUBJECT \"{}\"", fetch.pattern[0], fetch.pattern[1]),
        _ => unreachable!(),
    };

    // Format the from, to and year of the query
    query = format!("{} FROM \"{}\"", query, fetch.from);
    query = format!("{} TO \"{}\"", query, fetch.to);
    query = format!("{} SINCE \"01-Jan-{}\"", query, fetch.year);
    query = format!("{} BEFORE \"01-Jan-{}\"", query, fetch.year + 1);

    // Return the query
    Ok(query)
}

pub fn fetch_wrs(
    config: &MailConfig,
) -> imap::error::Result<Vec<Envelope>> {

    // Login to the IMAP server
    let mut imap_session = imap_login(&config.login)?;

    // Search for messages that contain the pattern
    let query = build_imap_search_query(&config.fetch)
        .map_err(|e| imap::error::Error::Bad(e))?;


    // List of WRs
    let mut wrs = Vec::new();

    for mailbox in config.fetch.wr_mailboxes.iter() {
        // Select the mailbox
        imap_session.select(mailbox)?;

        // Search for messages that contain the pattern
        let sequence_set = imap_session.search(query.as_str())?;
        let mut sequence_set: Vec<_> = sequence_set.into_iter().collect();
        sequence_set.sort();
        let sequence_set: String = join(sequence_set.into_iter().map(|s| s.to_string()), ",");
        // Fetch the messages
        let messages = imap_session.fetch(sequence_set, "ENVELOPE")?;

        // Print the subjects of the messages
        for message in messages.iter() {
            let envelope = message.envelope().unwrap();
            let reply_pattern= vec!["Re:", "RE:", "Aw:", "AW:"];

            match envelope.in_reply_to {
                None => {
                    let env = Envelope::from_imap_envelope(envelope);
                    wrs.push(env);
                },
                Some(_) => {
                    let subject = from_utf8(envelope.subject.unwrap()).expect("No subject in the envelope");
                    if reply_pattern.iter().any(|&s| subject.contains(s)) {
                        continue;
                    }
                    let env = Envelope::from_imap_envelope(envelope);
                    wrs.push(env);
                },
            };
        }
    }

    println!("Found {} WRs", wrs.len());

    imap_session.logout()?;
    Ok(wrs)
}

pub fn fetch_replies(
    config: &MailConfig,
) -> imap::error::Result<Vec<Envelope>> {

    // Login to the IMAP server
    let mut imap_session = imap_login(&config.login)?;


    let mut reply_fetch = config.fetch.clone();
    // Swap `from` and `to` in the fetch configuration
    std::mem::swap(&mut reply_fetch.from, &mut reply_fetch.to);

    // Search for messages that contain the pattern
    let query = build_imap_search_query(&reply_fetch)
        .map_err(|e| imap::error::Error::Bad(e))?;

    // List of WRs
    let mut wr_replies = Vec::new();

    for mailbox in config.fetch.re_mailboxes.iter() {
        // Select the mailbox
        imap_session.select(mailbox)?;

        // Search for messages that contain the pattern
        let sequence_set = imap_session.search(query.as_str())?;
        let mut sequence_set: Vec<_> = sequence_set.into_iter().collect();
        sequence_set.sort();
        let sequence_set: String = join(sequence_set.into_iter().map(|s| s.to_string()), ",");

        // Fetch the messages
        let messages = imap_session.fetch(sequence_set, "ENVELOPE")?;

        // Print the subjects of the messages
        for message in messages.iter() {
            let envelope = message.envelope().unwrap();

            match envelope.in_reply_to {
                Some(_) => {
                    let env = Envelope::from_imap_envelope(envelope);
                    wr_replies.push(env);
                    },
                None => continue,
            };
        }
    }

    println!("Found {} potential Replies", wr_replies.len());

    imap_session.logout()?;
    Ok(wr_replies)
}
