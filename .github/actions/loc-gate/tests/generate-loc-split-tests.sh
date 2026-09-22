#!/usr/bin/env bash
#
# Self-tests for the production-vs-test LoC split generator
# (ADORSYS-GIS/lightbridge-governance#173).
#
# Plain bash against a synthetic git repo — no bats, nothing beyond git, jq and
# python3 (all preinstalled on GitHub-hosted runners). Each case writes a known
# Rust file, runs the generator, and asserts on the emitted JSON.
#
# Layout: the tested script is `../generate-loc-split.sh` relative to this
# file's dir.
#
# Usage: generate-loc-split-tests.sh [generator-script]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GEN="${1:-${HERE}/../generate-loc-split.sh}"

pass=0
fail=0

REPO="$(mktemp -d "${TMPDIR:-/tmp}/loc-split-XXXXXX")"
OUT="${REPO}/split.json"

new_repo() {
  rm -rf "${REPO}"
  mkdir -p "${REPO}/crates/demo" "${REPO}/crates/demo/tests"
  (
    cd "${REPO}"
    git init -q
    git config user.email split@example.test
    git config user.name loc-split-tests
  ) >/dev/null
}

# run_gen [threshold] [roots...]: run the generator in the synthetic repo,
# writing to OUT. Small-file cases pass a low threshold so their files are
# included; the threshold/ranking cases use the default 200.
run_gen() {
  local threshold="${1:-200}"
  local roots="${2:-crates}"
  RUN_EXIT=0
  RUN_OUTPUT="$(
    cd "${REPO}" &&
      bash "${GEN}" "${threshold}" "${OUT}" "${roots}" 2>&1
  )" || RUN_EXIT=$?
}

check() {
  local name="$1" expect="$2" actual="$3"
  if [[ "${actual}" == "${expect}" ]]; then
    echo "PASS: ${name}"
    pass=$((pass + 1))
  else
    echo "FAIL: ${name}"
    echo "  expected: ${expect}"
    echo "  actual:   ${actual}"
    echo "  generator output:"
    sed 's/^/    /' <<<"${RUN_OUTPUT}"
    fail=$((fail + 1))
  fi
}

# jq_get <filter>: run jq against OUT, trimming whitespace.
jq_get() {
  jq -r "$1" "${OUT}"
}

# ================================================================= case 1
# Inline `#[cfg(test)] mod tests { ... }` block: the block's lines are test,
# everything else is production. 10 prod lines + a 5-line test module = 15 total.
new_repo
cat >"${REPO}/crates/demo/lib.rs" <<'EOF'
// line 1
// line 2
// line 3
// line 4
// line 5
// line 6
// line 7
// line 8
// line 9
// line 10
#[cfg(test)]
mod tests {
    // test line 1
    // test line 2
    // test line 3
}
EOF
run_gen 10
check "inline-mod: generator exits zero" "0" "${RUN_EXIT}"
check "inline-mod: total is 16" "16" "$(jq_get '.files[0].total')"
check "inline-mod: prod is 11" "11" "$(jq_get '.files[0].prod')"
check "inline-mod: test is 5" "5" "$(jq_get '.files[0].test')"

# ================================================================= case 2
# `#[cfg(test)] mod tests;` (external module, no brace) is NOT inline test —
# the sibling tests.rs is a separate file. All 12 lines are production.
new_repo
cat >"${REPO}/crates/demo/lib.rs" <<'EOF'
// line 1
// line 2
// line 3
// line 4
// line 5
// line 6
// line 7
// line 8
// line 9
// line 10
#[cfg(test)]
mod tests;
EOF
run_gen 10
check "external-mod: total is 12" "12" "$(jq_get '.files[0].total')"
check "external-mod: prod is 12 (no inline block)" "12" "$(jq_get '.files[0].prod')"
check "external-mod: test is 0" "0" "$(jq_get '.files[0].test')"

# ================================================================= case 3
# Inline `#[cfg(test)] fn foo() { ... }` is counted as test.
new_repo
cat >"${REPO}/crates/demo/lib.rs" <<'EOF'
// line 1
// line 2
// line 3
// line 4
// line 5
// line 6
// line 7
// line 8
// line 9
// line 10
#[cfg(test)]
fn helper() {
    // test line 1
    // test line 2
}
EOF
run_gen 10
check "inline-fn: total is 15" "15" "$(jq_get '.files[0].total')"
check "inline-fn: prod is 11" "11" "$(jq_get '.files[0].prod')"
check "inline-fn: test is 4" "4" "$(jq_get '.files[0].test')"

# ================================================================= case 4
# Attribute on its own line with the brace on the NEXT line is still counted.
new_repo
cat >"${REPO}/crates/demo/lib.rs" <<'EOF'
// line 1
// line 2
// line 3
// line 4
// line 5
// line 6
// line 7
// line 8
// line 9
// line 10
#[cfg(test)]
mod tests
{
    // test line 1
    // test line 2
}
EOF
run_gen 10
check "brace-next-line: total is 16" "16" "$(jq_get '.files[0].total')"
check "brace-next-line: prod is 11" "11" "$(jq_get '.files[0].prod')"
check "brace-next-line: test is 5" "5" "$(jq_get '.files[0].test')"

# ================================================================= case 5
# Nested braces inside a test block are balanced: the closing brace of an inner
# block must not terminate the outer test block early.
new_repo
cat >"${REPO}/crates/demo/lib.rs" <<'EOF'
// line 1
// line 2
// line 3
// line 4
// line 5
// line 6
// line 7
// line 8
// line 9
// line 10
#[cfg(test)]
mod tests {
    fn inner() {
        let _ = vec![1, 2, 3];
    }
    // test line 1
    // test line 2
}
EOF
run_gen 10
check "nested-braces: total is 18" "18" "$(jq_get '.files[0].total')"
check "nested-braces: prod is 11" "11" "$(jq_get '.files[0].prod')"
check "nested-braces: test is 7" "7" "$(jq_get '.files[0].test')"

# ================================================================= case 6
# Test-only files are excluded from the burn-down list: a `tests/` directory
# file and a `tests.rs` sibling are 100% test code and must not appear.
new_repo
cat >"${REPO}/crates/demo/lib.rs" <<'EOF'
// line 1
// line 2
// line 3
// line 4
// line 5
// line 6
// line 7
// line 8
// line 9
// line 10
// line 11
// line 12
// line 13
// line 14
// line 15
// line 16
// line 17
// line 18
// line 19
// line 20
// line 21
// line 22
// line 23
// line 24
// line 25
// line 26
// line 27
// line 28
// line 29
// line 30
// line 31
// line 32
// line 33
// line 34
// line 35
// line 36
// line 37
// line 38
// line 39
// line 40
// line 41
// line 42
// line 43
// line 44
// line 45
// line 46
// line 47
// line 48
// line 49
// line 50
// line 51
// line 52
// line 53
// line 54
// line 55
// line 56
// line 57
// line 58
// line 59
// line 60
// line 61
// line 62
// line 63
// line 64
// line 65
// line 66
// line 67
// line 68
// line 69
// line 70
// line 71
// line 72
// line 73
// line 74
// line 75
// line 76
// line 77
// line 78
// line 79
// line 80
// line 81
// line 82
// line 83
// line 84
// line 85
// line 86
// line 87
// line 88
// line 89
// line 90
// line 91
// line 92
// line 93
// line 94
// line 95
// line 96
// line 97
// line 98
// line 99
// line 100
// line 101
// line 102
// line 103
// line 104
// line 105
// line 106
// line 107
// line 108
// line 109
// line 110
// line 111
// line 112
// line 113
// line 114
// line 115
// line 116
// line 117
// line 118
// line 119
// line 120
// line 121
// line 122
// line 123
// line 124
// line 125
// line 126
// line 127
// line 128
// line 129
// line 130
// line 131
// line 132
// line 133
// line 134
// line 135
// line 136
// line 137
// line 138
// line 139
// line 140
// line 141
// line 142
// line 143
// line 144
// line 145
// line 146
// line 147
// line 148
// line 149
// line 150
// line 151
// line 152
// line 153
// line 154
// line 155
// line 156
// line 157
// line 158
// line 159
// line 160
// line 161
// line 162
// line 163
// line 164
// line 165
// line 166
// line 167
// line 168
// line 169
// line 170
// line 171
// line 172
// line 173
// line 174
// line 175
// line 176
// line 177
// line 178
// line 179
// line 180
// line 181
// line 182
// line 183
// line 184
// line 185
// line 186
// line 187
// line 188
// line 189
// line 190
// line 191
// line 192
// line 193
// line 194
// line 195
// line 196
// line 197
// line 198
// line 199
// line 200
// line 201
// line 202
// line 203
// line 204
// line 205
// line 206
// line 207
// line 208
// line 209
// line 210
// line 211
// line 212
// line 213
// line 214
// line 215
// line 216
// line 217
// line 218
// line 219
// line 220
// line 221
// line 222
// line 223
// line 224
// line 225
// line 226
// line 227
// line 228
// line 229
// line 230
// line 231
// line 232
// line 233
// line 234
// line 235
// line 236
// line 237
// line 238
// line 239
// line 240
// line 241
// line 242
// line 243
// line 244
// line 245
// line 246
// line 247
// line 248
// line 249
// line 250
EOF
# a 300-line integration test file and a 300-line tests.rs sibling
seq 1 300 >"${REPO}/crates/demo/tests/integration.rs"
seq 1 300 >"${REPO}/crates/demo/tests.rs"
run_gen
check "test-only-excluded: only lib.rs in the list" "1" "$(jq_get '.files | length')"
check "test-only-excluded: lib.rs present" "crates/demo/lib.rs" "$(jq_get '.files[0].path')"

# ================================================================= case 6b
# A file declared as an external `#[cfg(test)] mod <name>;` from another file is
# test-only and must be excluded, even though the cfg(test) gate lives in the
# DECLARING file (e.g. app/governance-ctl/src/test_support.rs, gated in main.rs).
new_repo
cat >"${REPO}/crates/demo/main.rs" <<'EOF'
// line 1
// line 2
// line 3
// line 4
// line 5
// line 6
// line 7
// line 8
// line 9
// line 10
// line 11
// line 12
// line 13
// line 14
// line 15
// line 16
// line 17
// line 18
// line 19
// line 20
// line 21
// line 22
// line 23
// line 24
// line 25
// line 26
// line 27
// line 28
// line 29
// line 30
// line 31
// line 32
// line 33
// line 34
// line 35
// line 36
// line 37
// line 38
// line 39
// line 40
// line 41
// line 42
// line 43
// line 44
// line 45
// line 46
// line 47
// line 48
// line 49
// line 50
// line 51
// line 52
// line 53
// line 54
// line 55
// line 56
// line 57
// line 58
// line 59
// line 60
// line 61
// line 62
// line 63
// line 64
// line 65
// line 66
// line 67
// line 68
// line 69
// line 70
// line 71
// line 72
// line 73
// line 74
// line 75
// line 76
// line 77
// line 78
// line 79
// line 80
// line 81
// line 82
// line 83
// line 84
// line 85
// line 86
// line 87
// line 88
// line 89
// line 90
// line 91
// line 92
// line 93
// line 94
// line 95
// line 96
// line 97
// line 98
// line 99
// line 100
// line 101
// line 102
// line 103
// line 104
// line 105
// line 106
// line 107
// line 108
// line 109
// line 110
// line 111
// line 112
// line 113
// line 114
// line 115
// line 116
// line 117
// line 118
// line 119
// line 120
// line 121
// line 122
// line 123
// line 124
// line 125
// line 126
// line 127
// line 128
// line 129
// line 130
// line 131
// line 132
// line 133
// line 134
// line 135
// line 136
// line 137
// line 138
// line 139
// line 140
// line 141
// line 142
// line 143
// line 144
// line 145
// line 146
// line 147
// line 148
// line 149
// line 150
// line 151
// line 152
// line 153
// line 154
// line 155
// line 156
// line 157
// line 158
// line 159
// line 160
// line 161
// line 162
// line 163
// line 164
// line 165
// line 166
// line 167
// line 168
// line 169
// line 170
// line 171
// line 172
// line 173
// line 174
// line 175
// line 176
// line 177
// line 178
// line 179
// line 180
// line 181
// line 182
// line 183
// line 184
// line 185
// line 186
// line 187
// line 188
// line 189
// line 190
// line 191
// line 192
// line 193
// line 194
// line 195
// line 196
// line 197
// line 198
// line 199
// line 200
// line 201
// line 202
// line 203
// line 204
// line 205
// line 206
// line 207
// line 208
// line 209
// line 210
// line 211
// line 212
// line 213
// line 214
// line 215
// line 216
// line 217
// line 218
// line 219
// line 220
// line 221
// line 222
// line 223
// line 224
// line 225
// line 226
// line 227
// line 228
// line 229
// line 230
// line 231
// line 232
// line 233
// line 234
// line 235
// line 236
// line 237
// line 238
// line 239
// line 240
// line 241
// line 242
// line 243
// line 244
// line 245
// line 246
// line 247
// line 248
// line 249
// line 250
#[cfg(test)]
mod test_support;
EOF
seq 1 250 >"${REPO}/crates/demo/test_support.rs"
run_gen 10
check "cfgtest-mod-excluded: only main.rs in the list" "1" "$(jq_get '.files | length')"
check "cfgtest-mod-excluded: main.rs present" "crates/demo/main.rs" "$(jq_get '.files[0].path')"
check "cfgtest-mod-excluded: test_support.rs absent" "false" "$(
  jq -r '[.files[].path] | index("crates/demo/test_support.rs") != null' "${OUT}"
)"

# ================================================================= case 6c
# A `pub(crate) mod <name>;` declared under #[cfg(test)] is also test-only and
# must be excluded — the declaring line starts with `pub`, not `mod`
# (e.g. app/governance-auth/src/managed/testutil.rs, declared in mod.rs).
new_repo
cat >"${REPO}/crates/demo/main.rs" <<'EOF'
// line 1
// line 2
// line 3
// line 4
// line 5
// line 6
// line 7
// line 8
// line 9
// line 10
// line 11
// line 12
// line 13
// line 14
// line 15
// line 16
// line 17
// line 18
// line 19
// line 20
// line 21
// line 22
// line 23
// line 24
// line 25
// line 26
// line 27
// line 28
// line 29
// line 30
// line 31
// line 32
// line 33
// line 34
// line 35
// line 36
// line 37
// line 38
// line 39
// line 40
// line 41
// line 42
// line 43
// line 44
// line 45
// line 46
// line 47
// line 48
// line 49
// line 50
// line 51
// line 52
// line 53
// line 54
// line 55
// line 56
// line 57
// line 58
// line 59
// line 60
// line 61
// line 62
// line 63
// line 64
// line 65
// line 66
// line 67
// line 68
// line 69
// line 70
// line 71
// line 72
// line 73
// line 74
// line 75
// line 76
// line 77
// line 78
// line 79
// line 80
// line 81
// line 82
// line 83
// line 84
// line 85
// line 86
// line 87
// line 88
// line 89
// line 90
// line 91
// line 92
// line 93
// line 94
// line 95
// line 96
// line 97
// line 98
// line 99
// line 100
// line 101
// line 102
// line 103
// line 104
// line 105
// line 106
// line 107
// line 108
// line 109
// line 110
// line 111
// line 112
// line 113
// line 114
// line 115
// line 116
// line 117
// line 118
// line 119
// line 120
// line 121
// line 122
// line 123
// line 124
// line 125
// line 126
// line 127
// line 128
// line 129
// line 130
// line 131
// line 132
// line 133
// line 134
// line 135
// line 136
// line 137
// line 138
// line 139
// line 140
// line 141
// line 142
// line 143
// line 144
// line 145
// line 146
// line 147
// line 148
// line 149
// line 150
// line 151
// line 152
// line 153
// line 154
// line 155
// line 156
// line 157
// line 158
// line 159
// line 160
// line 161
// line 162
// line 163
// line 164
// line 165
// line 166
// line 167
// line 168
// line 169
// line 170
// line 171
// line 172
// line 173
// line 174
// line 175
// line 176
// line 177
// line 178
// line 179
// line 180
// line 181
// line 182
// line 183
// line 184
// line 185
// line 186
// line 187
// line 188
// line 189
// line 190
// line 191
// line 192
// line 193
// line 194
// line 195
// line 196
// line 197
// line 198
// line 199
// line 200
// line 201
// line 202
// line 203
// line 204
// line 205
// line 206
// line 207
// line 208
// line 209
// line 210
// line 211
// line 212
// line 213
// line 214
// line 215
// line 216
// line 217
// line 218
// line 219
// line 220
// line 221
// line 222
// line 223
// line 224
// line 225
// line 226
// line 227
// line 228
// line 229
// line 230
// line 231
// line 232
// line 233
// line 234
// line 235
// line 236
// line 237
// line 238
// line 239
// line 240
// line 241
// line 242
// line 243
// line 244
// line 245
// line 246
// line 247
// line 248
// line 249
// line 250
#[cfg(test)]
pub(crate) mod testutil;
EOF
seq 1 250 >"${REPO}/crates/demo/testutil.rs"
run_gen 10
check "pub-mod-excluded: only main.rs in the list" "1" "$(jq_get '.files | length')"
check "pub-mod-excluded: main.rs present" "crates/demo/main.rs" "$(jq_get '.files[0].path')"
check "pub-mod-excluded: testutil.rs absent" "false" "$(
  jq -r '[.files[].path] | index("crates/demo/testutil.rs") != null' "${OUT}"
)"

# ================================================================= case 7
# Ranking is by PRODUCTION LoC descending, not total.
new_repo
# file A: 300 total, 100 prod (200 test) -> ranks lower by prod
cat >"${REPO}/crates/demo/a.rs" <<'EOF'
// line 1
// line 2
// line 3
// line 4
// line 5
// line 6
// line 7
// line 8
// line 9
// line 10
// line 11
// line 12
// line 13
// line 14
// line 15
// line 16
// line 17
// line 18
// line 19
// line 20
// line 21
// line 22
// line 23
// line 24
// line 25
// line 26
// line 27
// line 28
// line 29
// line 30
// line 31
// line 32
// line 33
// line 34
// line 35
// line 36
// line 37
// line 38
// line 39
// line 40
// line 41
// line 42
// line 43
// line 44
// line 45
// line 46
// line 47
// line 48
// line 49
// line 50
// line 51
// line 52
// line 53
// line 54
// line 55
// line 56
// line 57
// line 58
// line 59
// line 60
// line 61
// line 62
// line 63
// line 64
// line 65
// line 66
// line 67
// line 68
// line 69
// line 70
// line 71
// line 72
// line 73
// line 74
// line 75
// line 76
// line 77
// line 78
// line 79
// line 80
// line 81
// line 82
// line 83
// line 84
// line 85
// line 86
// line 87
// line 88
// line 89
// line 90
// line 91
// line 92
// line 93
// line 94
// line 95
// line 96
// line 97
// line 98
// line 99
// line 100
#[cfg(test)]
mod tests {
    // test 1
    // test 2
    // test 3
    // test 4
    // test 5
    // test 6
    // test 7
    // test 8
    // test 9
    // test 10
    // test 11
    // test 12
    // test 13
    // test 14
    // test 15
    // test 16
    // test 17
    // test 18
    // test 19
    // test 20
    // test 21
    // test 22
    // test 23
    // test 24
    // test 25
    // test 26
    // test 27
    // test 28
    // test 29
    // test 30
    // test 31
    // test 32
    // test 33
    // test 34
    // test 35
    // test 36
    // test 37
    // test 38
    // test 39
    // test 40
    // test 41
    // test 42
    // test 43
    // test 44
    // test 45
    // test 46
    // test 47
    // test 48
    // test 49
    // test 50
    // test 51
    // test 52
    // test 53
    // test 54
    // test 55
    // test 56
    // test 57
    // test 58
    // test 59
    // test 60
    // test 61
    // test 62
    // test 63
    // test 64
    // test 65
    // test 66
    // test 67
    // test 68
    // test 69
    // test 70
    // test 71
    // test 72
    // test 73
    // test 74
    // test 75
    // test 76
    // test 77
    // test 78
    // test 79
    // test 80
    // test 81
    // test 82
    // test 83
    // test 84
    // test 85
    // test 86
    // test 87
    // test 88
    // test 89
    // test 90
    // test 91
    // test 92
    // test 93
    // test 94
    // test 95
    // test 96
    // test 97
    // test 98
    // test 99
    // test 100
    // test 101
    // test 102
    // test 103
    // test 104
    // test 105
    // test 106
    // test 107
    // test 108
    // test 109
    // test 110
    // test 111
    // test 112
    // test 113
    // test 114
    // test 115
    // test 116
    // test 117
    // test 118
    // test 119
    // test 120
    // test 121
    // test 122
    // test 123
    // test 124
    // test 125
    // test 126
    // test 127
    // test 128
    // test 129
    // test 130
    // test 131
    // test 132
    // test 133
    // test 134
    // test 135
    // test 136
    // test 137
    // test 138
    // test 139
    // test 140
    // test 141
    // test 142
    // test 143
    // test 144
    // test 145
    // test 146
    // test 147
    // test 148
    // test 149
    // test 150
    // test 151
    // test 152
    // test 153
    // test 154
    // test 155
    // test 156
    // test 157
    // test 158
    // test 159
    // test 160
    // test 161
    // test 162
    // test 163
    // test 164
    // test 165
    // test 166
    // test 167
    // test 168
    // test 169
    // test 170
    // test 171
    // test 172
    // test 173
    // test 174
    // test 175
    // test 176
    // test 177
    // test 178
    // test 179
    // test 180
    // test 181
    // test 182
    // test 183
    // test 184
    // test 185
    // test 186
    // test 187
    // test 188
    // test 189
    // test 190
    // test 191
    // test 192
    // test 193
    // test 194
    // test 195
    // test 196
    // test 197
    // test 198
    // test 199
    // test 200
}
EOF
# file B: 250 total, 250 prod (no test) -> ranks higher by prod despite fewer total
seq 1 250 >"${REPO}/crates/demo/b.rs"
run_gen
check "rank-by-prod: two files listed" "2" "$(jq_get '.files | length')"
check "rank-by-prod: b.rs (250 prod) ranks above a.rs (100 prod)" "crates/demo/b.rs" "$(jq_get '.files[0].path')"
check "rank-by-prod: a.rs is second" "crates/demo/a.rs" "$(jq_get '.files[1].path')"

# ================================================================= case 8
# Files under the threshold are excluded entirely.
new_repo
seq 1 50 >"${REPO}/crates/demo/small.rs"
seq 1 250 >"${REPO}/crates/demo/big.rs"
run_gen
check "threshold: only the 250-line file is listed" "1" "$(jq_get '.files | length')"
check "threshold: big.rs present" "crates/demo/big.rs" "$(jq_get '.files[0].path')"

# ================================================================= case 9
# The artifact records the commit it was measured at (AC4).
new_repo
seq 1 250 >"${REPO}/crates/demo/big.rs"
(
  cd "${REPO}"
  git add -A
  git -c user.email=split@example.test -c user.name=t -c commit.gpgsign=false commit -q -m base
)
EXPECTED_SHA="$(git -C "${REPO}" rev-parse HEAD)"
run_gen
check "commit-key: equals HEAD" "${EXPECTED_SHA}" "$(jq_get '.commit')"

rm -rf "${REPO}"

echo
echo "loc-split generator self-tests: ${pass} passed, ${fail} failed"
[[ "${fail}" -eq 0 ]]
