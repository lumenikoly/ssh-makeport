.PHONY: dev build test lint fmt package

dev: ## Запустить приложение
	cargo run

build: ## Собрать приложение
	cargo build --release

test: ## Проверить ядро и удалённые команды
	cargo test --no-default-features
	python3 -m unittest discover -s tests -p 'test_*.py' -v

lint: ## Проверить Rust
	cargo clippy --all-targets

fmt: ## Форматировать Rust
	cargo fmt --all

package: build ## Упаковать приложение для текущей ОС
	python3 scripts/package.py
