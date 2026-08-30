run:
	cargo run --release

test_vfsmask:
	cargo test -p inforno_core test_vfsmask_permissions --release -- --nocapture
