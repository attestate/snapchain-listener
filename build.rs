fn main() {
    prost_build::Config::new()
        .compile_protos(
            &[
                "proto/onchain_event.proto",
                "proto/blocks.proto",
                "proto/request_response.proto",
                "proto/gossip.proto",
                "proto/message.proto",
                "proto/username_proof.proto",
            ],
            &["proto/"],
        )
        .unwrap();
}
