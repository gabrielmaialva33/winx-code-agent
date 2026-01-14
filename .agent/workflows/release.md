# Release

Prepara uma nova release do winx-code-agent.

## Instruções

1. Verifique se todos os testes passam (`cargo test`)
2. Verifique se não há warnings (`cargo clippy`)
3. Atualize a versão no `Cargo.toml`
4. Compile em release (`cargo build --release`)
5. Crie tag git com a versão
6. Gere changelog das mudanças
