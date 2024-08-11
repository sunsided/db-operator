//! # db-operator - CRD generator
//!
//! This utility application generates the CRD files required for Kubernetes.

#![forbid(unsafe_code)]

use controller::controllers::database::Database;
use controller::controllers::database_server::DatabaseServer;
use kube::CustomResourceExt;

fn main() {
    println!("---");
    println!("{}", serde_yaml::to_string(&DatabaseServer::crd()).unwrap());
    println!("---");
    print!("{}", serde_yaml::to_string(&Database::crd()).unwrap())
}
