use crate::firewall;
use crate::firewall::firewalld;
use crate::firewall::varktables;
use crate::firewall::varktables::types::TeardownPolicy::OnComplete;
use crate::firewall::varktables::types::{
    get_network_chains, get_port_forwarding_chains, TeardownPolicy,
};
use crate::network::internal_types::{
    PortForwardConfig, SetupNetwork, TearDownNetwork, TeardownPortForward,
};
use crate::network::types;
use futures::executor::block_on;
use iptables;
use iptables::IPTables;
use log::{debug, warn};
use std::error::Error;
use zbus::Connection;

pub(crate) const MAX_HASH_SIZE: usize = 13;

// Iptables driver - uses direct iptables commands via the iptables crate.
pub struct IptablesDriver {
    conn: IPTables,
    conn6: IPTables,
}

pub fn new() -> Result<Box<dyn firewall::FirewallDriver>, Box<dyn Error>> {
    // create an iptables connection
    let ipt = iptables::new(false)?;
    let ipt6 = iptables::new(true)?;
    let driver = IptablesDriver {
        conn: ipt,
        conn6: ipt6,
    };
    Ok(Box::new(driver))
}

impl firewall::FirewallDriver for IptablesDriver {
    fn setup_network(&self, network_setup: SetupNetwork) -> Result<(), Box<dyn Error>> {
        if let Some(subnet) = network_setup.net.subnets {
            for network in subnet {
                let is_ipv6 = network.subnet.network().is_ipv6();
                let mut conn = &self.conn;
                if is_ipv6 {
                    conn = &self.conn6;
                }
                let chains = varktables::types::get_network_chains(
                    conn,
                    network.subnet,
                    network_setup.network_hash_name.clone(),
                    is_ipv6,
                );

                for chain in chains {
                    chain.add_rules()?;
                }

                add_firewalld_if_possible(&network);
            }
        }
        Ok(())
    }

    // teardown_network should only be called in the case of
    // a complete teardown.
    fn teardown_network(&self, tear: TearDownNetwork) -> Result<(), Box<dyn Error>> {
        // Remove network specific general NAT rules
        if let Some(subnet) = tear.config.net.subnets {
            for network in subnet {
                let is_ipv6 = network.subnet.network().is_ipv6();
                let mut conn = &self.conn;
                if is_ipv6 {
                    conn = &self.conn6;
                }
                let chains = get_network_chains(
                    conn,
                    network.subnet,
                    tear.config.network_hash_name.clone(),
                    is_ipv6,
                );
                for chain in &chains {
                    // Because we only call teardown_network on complete teardown, we
                    // just send true here
                    chain.remove_rules(true)?;
                }

                for chain in chains {
                    match &chain.td_policy {
                        None => {}
                        Some(policy) => {
                            if tear.complete_teardown && *policy == OnComplete {
                                chain.remove()?;
                            }
                        }
                    }
                }

                if tear.complete_teardown {
                    rm_firewalld_if_possible(&network)
                }
            }
        }
        Result::Ok(())
    }

    fn setup_port_forward(&self, setup_portfw: PortForwardConfig) -> Result<(), Box<dyn Error>> {
        if let Some(v4) = setup_portfw.container_ip_v4 {
            let subnet_v4 = match setup_portfw.subnet_v4.clone() {
                Some(s) => s,
                None => {
                    return Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "ipv4 address but provided but no v4 subnet provided",
                    )))
                }
            };
            let chains =
                get_port_forwarding_chains(&self.conn, &setup_portfw, &v4, &subnet_v4, false);
            for chain in chains {
                chain.add_rules()?;
            }
        }
        if let Some(v6) = setup_portfw.container_ip_v6 {
            let subnet_v6 = match setup_portfw.subnet_v6.clone() {
                Some(s) => s,
                None => {
                    return Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "ipv6 address but provided but no v4 subnet provided",
                    )))
                }
            };
            let chains =
                get_port_forwarding_chains(&self.conn6, &setup_portfw, &v6, &subnet_v6, true);
            for chain in chains {
                chain.add_rules()?;
            }
        };
        Result::Ok(())
    }

    fn teardown_port_forward(&self, tear: TeardownPortForward) -> Result<(), Box<dyn Error>> {
        if let Some(v4) = tear.config.container_ip_v4 {
            let subnet_v4 = match tear.config.subnet_v4.clone() {
                Some(s) => s,
                None => {
                    return Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "ipv4 address but provided but no v4 subnet provided",
                    )))
                }
            };

            let chains =
                get_port_forwarding_chains(&self.conn, &tear.config, &v4, &subnet_v4, false);

            for chain in &chains {
                chain.remove_rules(tear.complete_teardown)?;
            }
            for chain in &chains {
                match &chain.td_policy {
                    None => {}
                    Some(policy) => {
                        if tear.complete_teardown && *policy == TeardownPolicy::OnComplete {
                            chain.remove()?;
                        }
                    }
                }
            }
        }

        if let Some(v6) = tear.config.container_ip_v6 {
            let subnet_v6 = match tear.config.subnet_v6.clone() {
                Some(s) => s,
                None => {
                    return Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "ipv6 address but provided but no v4 subnet provided",
                    )))
                }
            };

            let chains =
                get_port_forwarding_chains(&self.conn6, &tear.config, &v6, &subnet_v6, true);

            for chain in &chains {
                chain.remove_rules(tear.complete_teardown)?;
            }
            for chain in &chains {
                match &chain.td_policy {
                    None => {}
                    Some(policy) => {
                        if tear.complete_teardown && *policy == TeardownPolicy::OnComplete {
                            chain.remove()?;
                        }
                    }
                }
            }
        }
        Result::Ok(())
    }
}

// Check if firewalld is running
fn is_firewalld_running(conn: &Connection) -> bool {
    block_on(conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "GetNameOwner",
        &"org.fedoraproject.FirewallD1",
    ))
    .is_ok()
}

// If possible, add a firewalld rule to allow traffic.
// Ignore all errors, beyond possibly logging them.
fn add_firewalld_if_possible(net: &types::Subnet) {
    let conn = match block_on(Connection::system()) {
        Ok(conn) => conn,
        Err(_) => return,
    };
    if !is_firewalld_running(&conn) {
        return;
    }
    debug!(
        "Adding firewalld rules for network {}",
        net.subnet.to_string()
    );

    match firewalld::add_source_subnets_to_zone(&conn, "trusted", vec![net.clone()]) {
        Ok(_) => {}
        Err(e) => warn!(
            "Error adding subnet {} from firewalld trusted zone: {}",
            net.subnet.to_string(),
            e
        ),
    }
}

// If possible, remove a firewalld rule to allow traffic.
// Ignore all errors, beyond possibly logging them.
fn rm_firewalld_if_possible(net: &types::Subnet) {
    let conn = match block_on(Connection::system()) {
        Ok(conn) => conn,
        Err(_) => return,
    };
    if !is_firewalld_running(&conn) {
        return;
    }
    debug!(
        "Removing firewalld rules for IPs {}",
        net.subnet.to_string()
    );
    match block_on(conn.call_method(
        Some("org.fedoraproject.FirewallD1"),
        "/org/fedoraproject/FirewallD1",
        Some("org.fedoraproject.FirewallD1.zone"),
        "removeSource",
        &("trusted", net.subnet.to_string()),
    )) {
        Ok(_) => {}
        Err(e) => warn!(
            "Error removing subnet {} from firewalld trusted zone: {}",
            net.subnet.to_string(),
            e
        ),
    };
}
