# Contributing to ZGALAXY-RS

Thank you for your interest in contributing to **ZGALAXY-RS**! This project is an open-source, sovereign, memory-safe ZeroTier-compatible client and embedded controller licensed under the **GNU AGPL-3.0**.

---

## 📋 Table of Contents

1. [Code of Conduct](#code-of-conduct)
2. [Development Environment](#development-environment)
3. [Branching & Commit Guidelines](#branching--commit-guidelines)
4. [Testing & Quality Assurance](#testing--quality-assurance)
5. [Submitting a Pull Request](#submitting-a-pull-request)
6. [Licensing of Contributions](#licensing-of-contributions)

---

## 🤝 Code of Conduct

We are committed to providing a welcoming, inclusive, and harassment-free community. Please treat all contributors and maintainers with respect, fairness, and professionalism.

---

## 🛠️ Development Environment

### Prerequisites
* **Rust 1.75+** (`rustup default stable`)
* **Cargo** and `clippy` (`rustup component add clippy rustfmt`)
* Standard compilation tools for your OS (`build-essential`, `clang`, or MSVC on Windows)

### Building Locally
```bash
# Clone the repository
git clone https://github.com/dreamzone-cc/zgalaxy-rs.git
cd zgalaxy-rs

# Compile in debug mode
cargo build

# Run unit tests
cargo test
```

---

## 🌿 Branching & Commit Guidelines

* Create feature branches from `main`:
  ```bash
  git checkout -b feature/your-feature-name
  ```
* Use clear, descriptive commit messages following the Conventional Commits specification:
  * `feat: add IPv6 dual-stack resolution to dynamic resolver`
  * `fix: handle 0x05 rendezvous packet payload alignment`
  * `docs: update REST API endpoint definitions in README`

---

## 🧪 Testing & Quality Assurance

Before opening a pull request, ensure all tests pass and code is formatted cleanly:

```bash
# Format code
cargo fmt --all

# Run linter
cargo clippy -- -D warnings

# Execute test suite
cargo test --all
```

---

## 📬 Submitting a Pull Request

1. Push your branch to your fork on GitHub.
2. Open a Pull Request against `dreamzone-cc/zgalaxy-rs:main`.
3. Provide a clear description of the problem solved, changes made, and verification steps.
4. Ensure all CI/CD automated workflow checks pass.

---

## 📜 Licensing of Contributions

By contributing to ZGALAXY-RS, you agree that your contributions will be licensed under the **GNU Affero General Public License v3.0 (AGPL-3.0)**.
