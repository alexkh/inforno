run:
	cargo run --release

rus:
	cargo run --release -- --la ru

run_dark:
	cargo run --release -- --theme dark

run_light:
	cargo run --release -- --theme light

win:
	cargo build --target=x86_64-pc-windows-gnu --release
	rcedit target/x86_64-pc-windows-gnu/release/inforno.exe --set-icon assets/icon.ico

xwin:
	cargo xwin build --release --target x86_64-pc-windows-msvc
	rcedit target/x86_64-pc-windows-msvc/release/inforno.exe --set-icon assets/icon.ico

test:
	cargo test --release -- --nocapture
