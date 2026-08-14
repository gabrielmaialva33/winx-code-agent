#!/usr/bin/env ruby

require "yaml"

ROOT = File.expand_path("../..", __dir__)

def workflow(path)
  YAML.load_file(File.join(ROOT, path))
end

def triggers(document)
  document.fetch("on") { document.fetch(true) }
end

errors = []
check = ->(condition, message) { errors << message unless condition }

ci = workflow(".github/workflows/ci.yml")
ci_triggers = triggers(ci)
check.call(ci_triggers.key?("workflow_call"), "ci.yml must be reusable by release.yml")
check.call(!ci_triggers.fetch("push", {}).key?("tags"), "tag CI must run through release.yml only")

ci_jobs = ci.fetch("jobs")
check.call(ci_jobs.dig("windows", "runs-on") == "windows-latest", "CI must build on Windows")
windows_commands = ci_jobs.fetch("windows", {}).fetch("steps", []).filter_map { |step| step["run"] }.join("\n")
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

artifacts = jobs.dig("build", "strategy", "matrix", "include") || []
artifact_names = artifacts.filter_map { |entry| entry["asset_name"] }
check.call(artifact_names.include?("winx-linux-amd64.tar.gz"), "Linux release must be a bundle")
check.call(artifact_names.include?("winx-macos-arm64.tar.gz"), "macOS release must be a bundle")
check.call(artifact_names.include?("winx-windows-amd64.exe"), "Windows release must stay standalone")

build_commands = jobs.fetch("build", {}).fetch("steps", []).filter_map { |step| step["run"] }.join("\n")
check.call(build_commands.include?("cargo build --release --locked --bins"), "Unix build must compile every binary")
%w[winx-code-agent winxd winx-guardian].each do |binary|
  check.call(build_commands.include?(binary), "Unix bundle must include #{binary}")
end

check.call(jobs.dig("publish", "needs") == "build", "crates.io publish must wait for all builds")
release_needs = Array(jobs.dig("release", "needs"))
check.call(release_needs.sort == %w[build publish], "GitHub release must wait for build and publish")
check.call(!File.exist?(File.join(ROOT, ".github/workflows/publish.yml")), "parallel publish.yml must be removed")

unless errors.empty?
  warn errors.map { |error| "- #{error}" }.join("\n")
  exit 1
end

puts "release workflow contract: ok"
