#!/usr/bin/env ruby

require "yaml"

ROOT = File.expand_path("../..", __dir__)
PINNED_ACTION = %r{\A[^@]+@[0-9a-f]{40}\z}

def workflow(path)
  YAML.load_file(File.join(ROOT, path))
end

def triggers(document)
  document.fetch("on") { document.fetch(true) }
end

def steps(job)
  job.fetch("steps", [])
end

def commands(job)
  steps(job).filter_map { |step| step["run"] }.join("\n")
end

def action_uses(document)
  document.fetch("jobs").values.flat_map do |job|
    values = []
    values << job["uses"] if job["uses"]
    values.concat(steps(job).filter_map { |step| step["uses"] })
    values
  end
end

errors = []
check = ->(condition, message) { errors << message unless condition }

ci = workflow(".github/workflows/ci.yml")
ci_triggers = triggers(ci)
check.call(ci_triggers.key?("workflow_call"), "ci.yml must be reusable by release.yml")
check.call(ci_triggers.key?("schedule"), "CI must run scheduled dependency security checks")
check.call(!ci_triggers.fetch("push", {}).key?("tags"), "tag CI must run through release.yml only")

ci_jobs = ci.fetch("jobs")
%w[rust msrv pty security fuzz windows].each do |job|
  check.call(ci_jobs.key?(job), "CI must define the #{job} job")
end
check.call(ci_jobs.dig("windows", "runs-on") == "windows-latest", "CI must build on Windows")
check.call(commands(ci_jobs.fetch("msrv", {})).include?("cargo check --all-features --locked"), "MSRV job must check the complete feature graph")
check.call(commands(ci_jobs.fetch("pty", {})).include?("--ignored --test-threads=1"), "PTY job must run ignored real-terminal tests serially")
check.call(commands(ci_jobs.fetch("security", {})).include?("cargo audit --deny warnings"), "Security job must run cargo-audit")
check.call(commands(ci_jobs.fetch("fuzz", {})).include?("cargo +nightly fuzz build"), "Fuzz job must compile every fuzz target")
check.call(commands(ci_jobs.fetch("rust", {})).include?("cargo package --locked"), "CI must verify the crates.io package")
check.call(commands(ci_jobs.fetch("rust", {})).include?("cargo bench --bench performance --locked --no-run"), "CI must compile performance benchmarks")

windows_commands = commands(ci_jobs.fetch("windows", {}))
check.call(
  windows_commands.include?("cargo check --all-features --locked --bin winx-code-agent"),
  "Windows CI must check the supported client binary"
)
check.call(
  windows_commands.include?("cargo build --release --locked --bin winx-code-agent"),
  "Windows CI must build the supported release binary"
)

release = workflow(".github/workflows/release.yml")
jobs = release.fetch("jobs")
check.call(
  jobs.dig("quality", "uses") == "./.github/workflows/ci.yml",
  "release quality job must call ci.yml"
)
check.call(jobs.dig("build", "needs") == "quality", "release builds must wait for quality")
check.call(jobs.dig("sbom", "needs") == "quality", "SBOM generation must wait for quality")

artifacts = jobs.dig("build", "strategy", "matrix", "include") || []
expected_targets = {
  "x86_64-unknown-linux-gnu" => "winx-linux-amd64.tar.gz",
  "aarch64-apple-darwin" => "winx-macos-arm64.tar.gz",
  "x86_64-pc-windows-msvc" => "winx-windows-amd64.exe",
}
expected_targets.each do |target, asset|
  entry = artifacts.find { |candidate| candidate["target"] == target }
  check.call(!entry.nil?, "Release matrix must include #{target}")
  check.call(entry && entry["asset_name"] == asset, "#{target} must publish #{asset}")
  if target == "aarch64-apple-darwin"
    check.call(entry && entry["os"] == "macos-26", "arm64 macOS release must use the explicit macos-26 runner")
  end
end

build_commands = commands(jobs.fetch("build", {}))
check.call(
  build_commands.include?("cargo build --release --locked --target ${{ matrix.target }} --bins"),
  "Unix build must compile every binary for an explicit target"
)
%w[winx-code-agent winxd winx-guardian].each do |binary|
  check.call(build_commands.include?(binary), "Unix bundle must include #{binary}")
end
check.call(
  build_commands.include?("sha256sum") && build_commands.include?("shasum -a 256"),
  "Release build must generate portable artifact checksums"
)
check.call(
  action_uses(release).any? { |uses| uses.start_with?("actions/attest@") },
  "Release must create GitHub artifact attestations"
)

sbom_commands = commands(jobs.fetch("sbom", {}))
check.call(sbom_commands.include?("cargo cyclonedx --format json --all-features"), "Release must generate a JSON CycloneDX SBOM")

publish_needs = Array(jobs.dig("publish", "needs"))
check.call(publish_needs.sort == %w[build sbom], "crates.io publish must wait for builds and SBOM")
release_needs = Array(jobs.dig("release", "needs"))
check.call(release_needs.sort == %w[build publish sbom], "GitHub release must wait for build, SBOM, and publish")
check.call(commands(jobs.fetch("release", {})).include?("SHA256SUMS"), "GitHub release must include an aggregate checksum manifest")
check.call(!File.exist?(File.join(ROOT, ".github/workflows/publish.yml")), "parallel publish.yml must be removed")

(ci_jobs.values.flat_map { |job| steps(job).filter_map { |step| step["uses"] } } +
 action_uses(release)).each do |uses|
  next if uses.start_with?("./")

  check.call(PINNED_ACTION.match?(uses), "Action must be pinned to a full commit SHA: #{uses}")
end

unless errors.empty?
  warn errors.map { |error| "- #{error}" }.join("\n")
  exit 1
end

puts "release workflow contract: ok"
