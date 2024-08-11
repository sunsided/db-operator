use controller::DatabaseServer;
use kube::CustomResourceExt;

fn main() {
    print!("{}", serde_yaml::to_string(&DatabaseServer::crd()).unwrap())
}
