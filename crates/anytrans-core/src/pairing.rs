//! One-time pairing of two devices.
//!
//! The host shows a ticket (as text or a QR code) holding its address and a
//! random secret. The joiner dials that address and presents the secret. The
//! connection itself proves each side's device id, so afterwards both simply
//! remember the other's id.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use data_encoding::BASE32_NOPAD;
use iroh::{endpoint::Connection, Endpoint, EndpointAddr};
use serde::{Deserialize, Serialize};

use crate::{
    events::Event,
    node::Shared,
    peers::Peer,
    protocol::{read_frame, write_frame, PairRequest, PairResponse, ALPN_PAIR},
};

pub const TICKET_TTL: Duration = Duration::from_secs(300);
const TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct Pending {
    secret: [u8; 32],
    expires: Instant,
}

#[derive(Serialize, Deserialize)]
struct Ticket {
    addr: EndpointAddr,
    secret: [u8; 32],
}

pub(crate) fn create_ticket(shared: &Shared, addr: EndpointAddr) -> Result<String> {
    let secret: [u8; 32] = rand::random();
    *shared.pending_pair.lock().unwrap() = Some(Pending {
        secret,
        expires: Instant::now() + TICKET_TTL,
    });
    let bytes = postcard::to_stdvec(&Ticket { addr, secret })?;
    Ok(BASE32_NOPAD.encode(&bytes))
}

fn parse_ticket(text: &str) -> Result<Ticket> {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let bytes = BASE32_NOPAD
        .decode(cleaned.as_bytes())
        .context("not a pairing code")?;
    postcard::from_bytes(&bytes).context("not a pairing code")
}

/// Joiner side.
pub(crate) async fn join(shared: &Shared, endpoint: &Endpoint, ticket: &str) -> Result<Peer> {
    let ticket = parse_ticket(ticket)?;
    let host_id = ticket.addr.id;
    if host_id == endpoint.id() {
        bail!("this pairing code was created on this device");
    }
    shared.lookup.add_endpoint_info(ticket.addr.clone());

    let exchange = async {
        let conn = endpoint.connect(ticket.addr, ALPN_PAIR).await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        let request = PairRequest {
            secret: ticket.secret,
            name: shared.name.clone(),
            addr: endpoint.addr(),
        };
        write_frame(&mut send, &request).await?;
        send.finish()?;
        let response: PairResponse = read_frame(&mut recv).await?;
        conn.close(0u32.into(), b"paired");
        anyhow::Ok(response)
    };
    let response = tokio::time::timeout(TIMEOUT, exchange)
        .await
        .context("the other device did not answer")??;

    match response {
        PairResponse::Ok { name } => {
            let peer = Peer { id: host_id, name };
            shared.peers.upsert(peer.clone())?;
            shared.emit(Event::Paired { peer: peer.clone() });
            Ok(peer)
        }
        PairResponse::Denied => bail!("pairing code is wrong or has expired"),
    }
}

/// Host side.
pub(crate) async fn handle(shared: &Shared, conn: Connection) -> Result<()> {
    let (mut send, mut recv) = conn.accept_bi().await?;
    let request: PairRequest = read_frame(&mut recv).await?;

    let accepted = {
        let mut pending = shared.pending_pair.lock().unwrap();
        let ok = pending
            .as_ref()
            .is_some_and(|p| p.expires > Instant::now() && constant_eq(&p.secret, &request.secret));
        // A code works once, and a wrong guess burns it.
        *pending = None;
        ok
    };

    let mut paired = None;
    let response = if accepted {
        let peer = Peer {
            id: conn.remote_id(),
            name: request.name,
        };
        shared.peers.upsert(peer.clone())?;
        if request.addr.id == peer.id {
            shared.lookup.add_endpoint_info(request.addr);
        }
        paired = Some(peer);
        PairResponse::Ok {
            name: shared.name.clone(),
        }
    } else {
        PairResponse::Denied
    };
    write_frame(&mut send, &response).await?;
    send.finish()?;
    // Stay until the joiner has read the answer and hung up. Only then is it
    // safe to announce the pairing: a caller may shut the node down on hearing it.
    let _ = tokio::time::timeout(TIMEOUT, conn.closed()).await;
    if let Some(peer) = paired {
        shared.emit(Event::Paired { peer });
    }
    Ok(())
}

fn constant_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
