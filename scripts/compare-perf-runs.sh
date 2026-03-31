#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/compare-perf-runs.sh [options] BASE_RUN CANDIDATE_RUN

Compare two archived performance runs produced by scripts/archive-perf-run.sh.

Run arguments may be:
  - a run id under tmp/perf-records (for example 20260331-062033Z)
  - a path to an archived run directory
  - a path to benchmark-metrics.jsonl

Options:
  --archive-root PATH   Base directory for run ids. Default: tmp/perf-records
  --metric NAME         Metric to compare. Repeatable.
                        Default:
                          mean_ns
                          alloc_bytes
                          net_alloc_bytes
                          first_paint_alloc_bytes
                          first_interactive_alloc_bytes
                        Criterion aliases:
                          mean_ns, mean_upper_ns, median_ns, std_dev_ns
                        Sidecar examples:
                          avg_cpu_pct, rss_delta_kib, alloc_bytes,
                          net_alloc_bytes, first_paint_ms
  --kind NAME           Filter by benchmark kind: all, criterion, idle, launch
                        Default: all
  --direction MODE      Metric direction: lower, higher, neutral
                        Default: lower
  --sort FIELD          Sort rows by: regression, delta, abs_delta, name
                        Default: regression
  --limit N             Max rows per metric section. Default: 40
  --only-regressions    Show only worsening rows according to --direction
  -h, --help            Show this help.

Examples:
  scripts/compare-perf-runs.sh 20260331-010000Z 20260331-020000Z
  scripts/compare-perf-runs.sh --metric avg_cpu_pct --kind idle base-run new-run
  scripts/compare-perf-runs.sh --metric mean_ns --metric rss_delta_kib --only-regressions base new
EOF
}

require_jq() {
  if command -v jq >/dev/null 2>&1; then
    return 0
  fi

  echo "jq is required to compare archived performance runs." >&2
  exit 1
}

format_number() {
  awk -v value="$1" 'BEGIN {
    if (value == "" || value == "null") {
      printf "n/a";
    } else if (value == int(value)) {
      printf "%.0f", value;
    } else {
      printf "%.3f", value;
    }
  }'
}

format_percent() {
  awk -v value="$1" 'BEGIN {
    if (value == "" || value == "null") {
      printf "n/a";
    } else {
      printf "%.2f%%", value;
    }
  }'
}

format_bytes() {
  awk -v value="$1" 'BEGIN {
    abs = value;
    if (abs < 0) abs = -abs;
    if (value == "" || value == "null") {
      printf "n/a";
    } else if (abs >= 1073741824) {
      printf "%.3f GiB", value / 1073741824;
    } else if (abs >= 1048576) {
      printf "%.3f MiB", value / 1048576;
    } else if (abs >= 1024) {
      printf "%.3f KiB", value / 1024;
    } else {
      printf "%.0f B", value;
    }
  }'
}

format_ms() {
  awk -v value="$1" 'BEGIN {
    if (value == "" || value == "null") {
      printf "n/a";
    } else {
      printf "%.3f ms", value;
    }
  }'
}

format_kib() {
  awk -v value="$1" 'BEGIN {
    abs = value;
    if (abs < 0) abs = -abs;
    if (value == "" || value == "null") {
      printf "n/a";
    } else if (abs >= 1048576) {
      printf "%.3f GiB", value / 1048576;
    } else if (abs >= 1024) {
      printf "%.3f MiB", value / 1024;
    } else {
      printf "%.0f KiB", value;
    }
  }'
}

format_pct() {
  awk -v value="$1" 'BEGIN {
    if (value == "" || value == "null") {
      printf "n/a";
    } else {
      printf "%.3f%%", value;
    }
  }'
}

format_duration_ns() {
  awk -v ns="$1" 'BEGIN {
    if (ns == "" || ns == "null") {
      printf "n/a";
    } else if (ns >= 1000000 || ns <= -1000000) {
      printf "%.3f ms", ns / 1000000;
    } else if (ns >= 1000 || ns <= -1000) {
      printf "%.3f us", ns / 1000;
    } else {
      printf "%.0f ns", ns;
    }
  }'
}

metric_key() {
  case "$1" in
    mean_ns|criterion.mean_ns)
      printf 'criterion.mean_ns\n'
      ;;
    mean_upper_ns|criterion.mean_upper_ns)
      printf 'criterion.mean_upper_ns\n'
      ;;
    median_ns|criterion.median_ns)
      printf 'criterion.median_ns\n'
      ;;
    std_dev_ns|criterion.std_dev_ns)
      printf 'criterion.std_dev_ns\n'
      ;;
    *)
      printf 'sidecar:%s\n' "$1"
      ;;
  esac
}

metric_label() {
  case "$1" in
    criterion.mean_ns)
      printf 'criterion mean\n'
      ;;
    criterion.mean_upper_ns)
      printf 'criterion mean 95%% upper\n'
      ;;
    criterion.median_ns)
      printf 'criterion median\n'
      ;;
    criterion.std_dev_ns)
      printf 'criterion std dev\n'
      ;;
    sidecar:*)
      printf 'sidecar %s\n' "${1#sidecar:}"
      ;;
  esac
}

metric_is_duration_ns() {
  [[ "$1" == criterion.*_ns || "$1" == criterion.mean_ns || "$1" == criterion.median_ns ]]
}

metric_pretty_direction() {
  case "$1" in
    lower)
      printf 'lower is better\n'
      ;;
    higher)
      printf 'higher is better\n'
      ;;
    *)
      printf 'change only\n'
      ;;
  esac
}

format_metric_value() {
  local metric="$1"
  local value="$2"

  if metric_is_duration_ns "${metric}"; then
    format_duration_ns "${value}"
  elif [[ "${metric}" == sidecar:*_bytes ]]; then
    format_bytes "${value}"
  elif [[ "${metric}" == sidecar:*_kib ]]; then
    format_kib "${value}"
  elif [[ "${metric}" == sidecar:*_ms ]]; then
    format_ms "${value}"
  elif [[ "${metric}" == sidecar:*_pct ]]; then
    format_pct "${value}"
  else
    format_number "${value}"
  fi
}

metadata_get() {
  local file="$1"
  local key="$2"
  awk -F': ' -v wanted="$key" '$1 == wanted { print substr($0, index($0, ": ") + 2); exit }' "$file"
}

abs_path() {
  local path="$1"
  if [[ -d "$path" ]]; then
    (cd "$path" && pwd)
  else
    local parent
    local base
    parent="$(cd "$(dirname "$path")" && pwd)"
    base="$(basename "$path")"
    printf '%s/%s\n' "$parent" "$base"
  fi
}

resolve_run() {
  local input="$1"
  local run_dir=""
  local jsonl_path=""
  local metadata_path=""

  if [[ -f "$input" ]]; then
    jsonl_path="$(abs_path "$input")"
    run_dir="$(dirname "$jsonl_path")"
  elif [[ -d "$input" ]]; then
    run_dir="$(abs_path "$input")"
    jsonl_path="${run_dir}/benchmark-metrics.jsonl"
  else
    run_dir="$(abs_path "${archive_root%/}/${input}")"
    jsonl_path="${run_dir}/benchmark-metrics.jsonl"
  fi

  metadata_path="${run_dir}/metadata.txt"

  if [[ ! -f "${jsonl_path}" ]]; then
    echo "Missing benchmark metrics file: ${jsonl_path}" >&2
    exit 1
  fi
  if [[ ! -f "${metadata_path}" ]]; then
    echo "Missing metadata file: ${metadata_path}" >&2
    exit 1
  fi

  printf '%s|%s|%s\n' "${run_dir}" "${jsonl_path}" "${metadata_path}"
}

print_run_summary() {
  local label="$1"
  local run_dir="$2"
  local metadata_path="$3"
  local run_id
  local git_head
  local git_branch
  local runner_class
  local real_repo_root

  run_id="$(metadata_get "${metadata_path}" "run_id")"
  git_head="$(metadata_get "${metadata_path}" "git_head")"
  git_branch="$(metadata_get "${metadata_path}" "git_branch")"
  runner_class="$(metadata_get "${metadata_path}" "runner_class")"
  real_repo_root="$(metadata_get "${metadata_path}" "real_repo_root")"

  echo "${label}: ${run_id:-$(basename "${run_dir}")}"
  echo "  dir: ${run_dir}"
  echo "  git: ${git_branch:-unknown} ${git_head:-unknown}"
  echo "  runner_class: ${runner_class:-<unset>}"
  echo "  real_repo_root: ${real_repo_root:-<unset>}"
}

compare_metric() {
  local metric="$1"
  local metric_jq="$2"
  local tmp_file
  local matched_rows
  local total_rows
  local only_regressions_json="false"

  tmp_file="$(mktemp)"
  if [[ ${only_regressions} -eq 1 ]]; then
    only_regressions_json="true"
  fi

  jq -s \
    --slurpfile cand "${candidate_jsonl}" \
    --arg metric "${metric_jq}" \
    --arg kind "${kind_filter}" \
    --arg direction "${direction}" \
    --arg sort_by "${sort_by}" \
    --argjson limit "${limit}" \
    --argjson only_regressions "${only_regressions_json}" '
      def idx(xs):
        reduce xs[] as $x ({}; .[$x.kind + "|" + $x.bench] = $x);

      def metric_value($row; $metric):
        if $metric == "criterion.mean_ns" then $row.criterion.mean_ns
        elif $metric == "criterion.mean_upper_ns" then $row.criterion.mean_upper_ns
        elif $metric == "criterion.median_ns" then $row.criterion.median_ns
        elif $metric == "criterion.std_dev_ns" then $row.criterion.std_dev_ns
        elif ($metric | startswith("sidecar:")) then $row.metrics[$metric | ltrimstr("sidecar:")]
        else null
        end;

      def status_for($direction; $delta):
        if $delta == 0 then
          "unchanged"
        elif $direction == "lower" then
          (if $delta < 0 then "improved" else "regressed" end)
        elif $direction == "higher" then
          (if $delta > 0 then "improved" else "regressed" end)
        else
          "changed"
        end;

      def is_regression($direction; $delta):
        ($direction == "lower" and $delta > 0) or
        ($direction == "higher" and $delta < 0) or
        ($direction == "neutral" and $delta != 0);

      . as $base
      | (idx($base)) as $base_idx
      | (idx($cand)) as $cand_idx
      | [
          $base_idx
          | keys_unsorted[]
          | select($cand_idx[.] != null)
          | ($base_idx[.]) as $old
          | ($cand_idx[.]) as $new
          | select($kind == "all" or $old.kind == $kind)
          | (metric_value($old; $metric)) as $before
          | (metric_value($new; $metric)) as $after
          | select($before != null and $after != null)
          | {
              bench: $old.bench,
              kind: $old.kind,
              before: $before,
              after: $after,
              delta: ($after - $before),
              delta_pct: (if $before == 0 then null else (($after - $before) / $before * 100) end),
              status: status_for($direction; ($after - $before))
            }
        ] as $rows
      | ($rows | length) as $matched
      | {
          improved: ([ $rows[] | select(.status == "improved") ] | length),
          regressed: ([ $rows[] | select(.status == "regressed") ] | length),
          unchanged: ([ $rows[] | select(.status == "unchanged") ] | length),
          changed: ([ $rows[] | select(.status == "changed") ] | length)
        } as $summary
      | (
          if $only_regressions then
            [
              $rows[]
              | select(is_regression($direction; .delta))
            ]
          else
            $rows
          end
        ) as $filtered
      | (
          if $sort_by == "name" then
            ($filtered | sort_by(.bench, .kind))
          elif $sort_by == "delta" then
            ($filtered
             | map(. + {
                 score: (
                   if $direction == "higher" then -(.delta)
                   elif $direction == "neutral" then (.delta | if . < 0 then -. else . end)
                   else .delta
                   end
                 )
               })
             | sort_by(.score, .bench, .kind)
             | reverse
             | map(del(.score)))
          elif $sort_by == "abs_delta" then
            ($filtered
             | map(. + { score: (.delta | if . < 0 then -. else . end) })
             | sort_by(.score, .bench, .kind)
             | reverse
             | map(del(.score)))
          else
            ($filtered
             | map(. + {
                 score: (
                   if .delta_pct == null then
                     -1e308
                   elif $direction == "higher" then
                     -(.delta_pct)
                   elif $direction == "neutral" then
                     (.delta_pct | if . < 0 then -. else . end)
                   else
                     .delta_pct
                   end
                 )
               })
             | sort_by(.score, .bench, .kind)
             | reverse
             | map(del(.score)))
          end
        ) as $sorted
      | {
          matched: $matched,
          summary: $summary,
          shown: (($sorted | length) | if . > $limit then $limit else . end),
          rows: ($sorted[:$limit])
        }
    ' "${base_jsonl}" > "${tmp_file}"

  matched_rows="$(jq -r '.matched' "${tmp_file}")"
  total_rows="$(jq -r '.shown' "${tmp_file}")"

  echo
  echo "Metric: $(metric_label "${metric_jq}")"
  echo "  interpretation: $(metric_pretty_direction "${direction}")"
  echo "  matched benchmarks: ${matched_rows}"
  echo "  shown rows: ${total_rows}"

  if [[ "${matched_rows}" == "0" ]]; then
    echo "  no comparable benchmarks found"
    rm -f "${tmp_file}"
    return 0
  fi

  echo "  summary: improved=$(jq -r '.summary.improved' "${tmp_file}") regressed=$(jq -r '.summary.regressed' "${tmp_file}") unchanged=$(jq -r '.summary.unchanged' "${tmp_file}") changed=$(jq -r '.summary.changed' "${tmp_file}")"

  printf '%-12s  %-46s  %-11s  %-16s  %-16s  %-16s  %-10s\n' "kind" "bench" "status" "before" "after" "delta" "delta %"
  printf '%-12s  %-46s  %-11s  %-16s  %-16s  %-16s  %-10s\n' "------------" "----------------------------------------------" "-----------" "----------------" "----------------" "----------------" "----------"

  while IFS=$'\t' read -r row_kind row_bench row_status row_before row_after row_delta row_delta_pct; do
    local before_fmt
    local after_fmt
    local delta_fmt

    before_fmt="$(format_metric_value "${metric_jq}" "${row_before}")"
    after_fmt="$(format_metric_value "${metric_jq}" "${row_after}")"
    delta_fmt="$(format_metric_value "${metric_jq}" "${row_delta}")"

    printf '%-12s  %-46s  %-11s  %-16s  %-16s  %-16s  %-10s\n' \
      "${row_kind}" \
      "${row_bench:0:46}" \
      "${row_status}" \
      "${before_fmt}" \
      "${after_fmt}" \
      "${delta_fmt}" \
      "$(format_percent "${row_delta_pct}")"
  done < <(
    jq -r '.rows[] | [.kind, .bench, .status, (.before|tostring), (.after|tostring), (.delta|tostring), ((.delta_pct // "null")|tostring)] | @tsv' "${tmp_file}"
  )

  rm -f "${tmp_file}"
}

archive_root="tmp/perf-records"
kind_filter="all"
direction="lower"
sort_by="regression"
limit=40
only_regressions=0
metrics=()
positionals=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive-root)
      archive_root="$2"
      shift 2
      ;;
    --metric)
      metrics+=("$2")
      shift 2
      ;;
    --kind)
      kind_filter="$2"
      shift 2
      ;;
    --direction)
      direction="$2"
      shift 2
      ;;
    --sort)
      sort_by="$2"
      shift 2
      ;;
    --limit)
      limit="$2"
      shift 2
      ;;
    --only-regressions)
      only_regressions=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      positionals+=("$1")
      shift
      ;;
  esac
done

if [[ ${#metrics[@]} -eq 0 ]]; then
  metrics=(
    "mean_ns"
    "alloc_bytes"
    "net_alloc_bytes"
    "first_paint_alloc_bytes"
    "first_interactive_alloc_bytes"
  )
fi

case "${kind_filter}" in
  all|criterion|idle|launch)
    ;;
  *)
    echo "Unknown --kind value: ${kind_filter}" >&2
    exit 2
    ;;
esac

case "${direction}" in
  lower|higher|neutral)
    ;;
  *)
    echo "Unknown --direction value: ${direction}" >&2
    exit 2
    ;;
esac

case "${sort_by}" in
  regression|delta|abs_delta|name)
    ;;
  *)
    echo "Unknown --sort value: ${sort_by}" >&2
    exit 2
    ;;
esac

if ! [[ "${limit}" =~ ^[0-9]+$ ]] || [[ "${limit}" -lt 1 ]]; then
  echo "--limit must be a positive integer" >&2
  exit 2
fi

if [[ ${#positionals[@]} -ne 2 ]]; then
  usage >&2
  exit 2
fi

require_jq

IFS='|' read -r base_run_dir base_jsonl base_metadata <<< "$(resolve_run "${positionals[0]}")"
IFS='|' read -r candidate_run_dir candidate_jsonl candidate_metadata <<< "$(resolve_run "${positionals[1]}")"

echo "Comparing archived performance runs"
print_run_summary "base" "${base_run_dir}" "${base_metadata}"
print_run_summary "candidate" "${candidate_run_dir}" "${candidate_metadata}"
echo "filters: kind=${kind_filter} direction=${direction} sort=${sort_by} limit=${limit} only_regressions=${only_regressions}"

base_git_head="$(metadata_get "${base_metadata}" "git_head")"
candidate_git_head="$(metadata_get "${candidate_metadata}" "git_head")"
base_runner_class="$(metadata_get "${base_metadata}" "runner_class")"
candidate_runner_class="$(metadata_get "${candidate_metadata}" "runner_class")"
base_real_repo_root="$(metadata_get "${base_metadata}" "real_repo_root")"
candidate_real_repo_root="$(metadata_get "${candidate_metadata}" "real_repo_root")"

if [[ "${base_git_head}" != "${candidate_git_head}" ]]; then
  echo "warning: git_head differs between runs"
fi
if [[ "${base_runner_class}" != "${candidate_runner_class}" ]]; then
  echo "warning: runner_class differs between runs"
fi
if [[ "${base_real_repo_root}" != "${candidate_real_repo_root}" ]]; then
  echo "warning: real_repo_root differs between runs"
fi

for metric in "${metrics[@]}"; do
  compare_metric "${metric}" "$(metric_key "${metric}")"
done
