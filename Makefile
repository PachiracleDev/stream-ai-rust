# Un solo objetivo: levantar el servicio (Redis va aparte; .env con REDIS_URL, etc.)
.PHONY: run
export PATH := $(HOME)/.cargo/bin:$(PATH)

run:
	@test -f .env || { echo "Falta .env en el directorio del proyecto."; exit 1; }
	@command -v cargo >/dev/null 2>&1 || { echo "No se encuentra cargo. Instala Rust: https://rustup.rs"; exit 1; }
	cargo run
