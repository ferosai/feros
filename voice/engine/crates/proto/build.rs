use pbjson_build::Builder;
use prost_build::Config;
use std::env;
use std::path::{Path, PathBuf};

fn main() {
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let dest_path = PathBuf::from(out_dir.clone());

    // From voice/engine/crates/proto up to voice-agent-os/proto
    let proto_dir = Path::new("../../../../proto");
    let stt_proto = proto_dir.join("stt.proto");
    let agent_proto = proto_dir.join("agent.proto");

    println!("cargo:rerun-if-changed={}", stt_proto.display());
    println!("cargo:rerun-if-changed={}", agent_proto.display());

    let mut config = Config::new();
    config.out_dir(&dest_path);

    let descriptor_path = dest_path.join("proto_descriptor.bin");
    config
        .file_descriptor_set_path(&descriptor_path)
        .compile_protos(&[stt_proto, agent_proto], &[proto_dir])
        .expect("Protobuf compilation failed.");

    let descriptor_set = std::fs::read(&descriptor_path).unwrap();

    Builder::new()
        .register_descriptors(&descriptor_set)
        .unwrap()
        .out_dir(&dest_path)
        .build(&[".stt", ".agent"])
        .unwrap();
}
