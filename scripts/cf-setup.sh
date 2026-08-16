#!/usr/bin/env bash
# ============================================================================
# cf-setup.sh — Cloudflare IPv6 直连 / 302 跳转 一键配置（Linux / macOS / Termux）
#
# 配套说明：同目录 CF-SETUP.md
# 依赖：curl + jq（Debian/Ubuntu: apt install curl jq; Termux: pkg install curl jq;
#       macOS: brew install jq）
#
# 用法：
#   ./cf-setup.sh                       # 交互式配置
#   ./cf-setup.sh --ddns                # DDNS 刷新（cron 用，按已保存配置执行）
#   ./cf-setup.sh --mode direct --token <t> --zone example.com --label ilink \
#                 --port 8888 --scheme http --ip <IPv6> --yes   # 全自动
# ============================================================================
set -u

API="https://api.cloudflare.com/client/v4"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CONFIG_PATH="$SCRIPT_DIR/cf-config.json"
RULE_MARKER="ilink-cf-ddns"

MODE=""; TOKEN="${CF_API_TOKEN:-}"; ZONE=""; LABEL=""; PORT=""; SCHEME=""; IP=""
DDNS=0; YES=0

c_step() { printf '\n\033[36m==> %s\033[0m\n' "$1"; }
c_ok()   { printf '  [OK] %s\n' "$1"; }
c_info() { printf '  ..  %s\n' "$1"; }
c_warn() { printf '  \033[33m[!]\033[0m  %s\n' "$1"; }
c_fail() { printf '  \033[31m[X]\033[0m  %s\n' "$1"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --mode)   MODE="$2"; shift 2 ;;
    --token)  TOKEN="$2"; shift 2 ;;
    --zone)   ZONE="$2"; shift 2 ;;
    --label)  LABEL="$2"; shift 2 ;;
    --port)   PORT="$2"; shift 2 ;;
    --scheme) SCHEME="$2"; shift 2 ;;
    --ip)     IP="$2"; shift 2 ;;
    --ddns)   DDNS=1; shift ;;
    --yes)    YES=1; shift ;;
    *) echo "未知参数: $1"; exit 1 ;;
  esac
done

for bin in curl jq; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "缺少依赖: $bin（apt install $bin / pkg install $bin / brew install $bin）"
    exit 1
  fi
done

read_default() { # $1=提示 $2=默认值
  if [ "$YES" -eq 1 ]; then printf '%s' "$2"; return; fi
  printf '%s [%s]: ' "$1" "$2" >&2
  read -r v
  [ -z "$v" ] && printf '%s' "$2" || printf '%s' "$v"
}

read_choice() { # $1=提示 $2..$n=选项；输出选中项
  local prompt="$1"; shift
  local opts=("$@")
  local i=1
  for o in "${opts[@]}"; do printf '  %d) %s\n' "$i" "$o" >&2; i=$((i+1)); done
  while :; do
    if [ "$YES" -eq 1 ]; then printf '%s' "${opts[0]}"; return; fi
    printf '%s (1-%d，默认 1): ' "$prompt" "${#opts[@]}" >&2
    read -r n
    [ -z "$n" ] && { printf '%s' "${opts[0]}"; return; }
    case "$n" in
      *[!0-9]*|"") : ;;
      *) if [ "$n" -ge 1 ] && [ "$n" -le "${#opts[@]}" ]; then printf '%s' "${opts[$((n-1))]}"; return; fi ;;
    esac
    printf '  无效输入\n' >&2
  done
}

cf_request() { # $1=METHOD $2=Path $3=JSON body(可空)
  local method="$1" path="$2" body="${3:-}"
  if [ -n "$body" ]; then
    curl -sS -X "$method" "$API$path" \
      -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
      -d "$body"
  else
    curl -sS -X "$method" "$API$path" \
      -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json"
  fi
}

# 枚举本机公网 IPv6（排除 lo / fe80::/10 / ULA fc00::/7），优先稳定地址
detect_ipv6() {
  local out=""
  if command -v ip >/dev/null 2>&1; then
    out=$(ip -6 -o addr show scope global 2>/dev/null | awk '{split($4,a,"/"); print a[1]}')
  fi
  if [ -z "$out" ] && command -v ifconfig >/dev/null 2>&1; then
    out=$(ifconfig 2>/dev/null | awk '/inet6/ {print $2}' | sed 's/%[a-z0-9]*//')
  fi
  printf '%s\n' "$out" | grep -v '^$' | grep -v '^::1$' | grep -v '^fe80' \
    | grep -v '^fc' | grep -v '^fd' | awk '!seen[$0]++'
}

is_valid_ipv6() { echo "$1" | grep -Eq '^[0-9a-fA-F:]+$' && echo "$1" | grep -q ':'; }

upsert_aaaa() { # $1=zone_id $2=fqdn $3=ip $4=proxied(true/false) → 输出 record_id
  local zone_id="$1" fqdn="$2" ip="$3" proxied="$4"
  local existing rec_id body method path resp
  existing=$(cf_request GET "/zones/$zone_id/dns_records?name=$fqdn&type=AAAA" | jq -r '.result')
  rec_id=$(printf '%s' "$existing" | jq -r 'if type=="array" and length>0 then .[0].id else "" end')
  body=$(jq -n --arg n "$fqdn" --arg c "$ip" --argjson p "$proxied" \
    '{type:"AAAA",name:$n,content:$c,ttl:60,proxied:$p,comment:"ilink-wm cf-setup"}')
  if [ -n "$rec_id" ]; then
    local cur_ip cur_px
    cur_ip=$(printf '%s' "$existing" | jq -r '.[0].content')
    cur_px=$(printf '%s' "$existing" | jq -r '.[0].proxied')
    if [ "$cur_ip" = "$ip" ] && [ "$cur_px" = "$proxied" ]; then
      c_ok "AAAA 记录已是最新：$fqdn -> $ip"
      printf '%s' "$rec_id"; return 0
    fi
    method=PATCH; path="/zones/$zone_id/dns_records/$rec_id"
  else
    method=POST; path="/zones/$zone_id/dns_records"
  fi
  resp=$(cf_request "$method" "$path" "$body")
  if [ "$(printf '%s' "$resp" | jq -r '.success')" != "true" ]; then
    # 某些套餐不允许 TTL=60，回退 auto 重试
    body=$(printf '%s' "$body" | jq '.ttl=1')
    resp=$(cf_request "$method" "$path" "$body")
  fi
  if [ "$(printf '%s' "$resp" | jq -r '.success')" != "true" ]; then
    c_fail "AAAA upsert 失败：$(printf '%s' "$resp" | jq -c '.errors')"; return 1
  fi
  c_ok "AAAA 记录已写入：$fqdn -> $ip (proxied=$proxied)"
  printf '%s' "$resp" | jq -r '.result.id'
}

# 302 跳转规则 upsert：只动带标记的规则，其他规则保留。
# 目标 URL 用表达式字符串字面量（"http://[v6]:port"）规避静态 URL 校验兼容问题。
upsert_redirect_rule() { # $1=zone_id $2=fqdn $3=target → 输出 ruleset_id
  local zone_id="$1" fqdn="$2" target="$3"
  local rs rs_id others merged resp
  rs=$(cf_request GET "/zones/$zone_id/rulesets/phases/http_request_dynamic_redirect/entrypoint")
  rs_id=$(printf '%s' "$rs" | jq -r 'if .success==true then .result.id else "" end')
  if [ -z "$rs_id" ]; then
    rs=$(cf_request POST "/zones/$zone_id/rulesets" \
      '{"name":"ilink redirect","kind":"zone","phase":"http_request_dynamic_redirect"}')
    rs_id=$(printf '%s' "$rs" | jq -r 'if .success==true then .result.id else "" end')
    if [ -z "$rs_id" ]; then
      c_fail "创建 redirect ruleset 失败：$(printf '%s' "$rs" | jq -c '.errors')"; return 1
    fi
  fi
  others=$(printf '%s' "$rs" | jq -c '[.result.rules[]? | select(.description != "ilink-cf-ddns")]')
  merged=$(jq -n --argjson others "$others" --arg fqdn "$fqdn" --arg target "$target" '
    {rules: ($others + [{
      expression: ("(http.host eq \"" + $fqdn + "\")"),
      action: "redirect",
      action_parameters: {from_value: {
        status_code: 302,
        target_url: {expression: ("\"" + $target + "\"")},
        preserve_query_string: true
      }},
      description: "ilink-cf-ddns",
      enabled: true
    }])}')
  resp=$(cf_request PUT "/zones/$zone_id/rulesets/$rs_id/rules" "$merged")
  if [ "$(printf '%s' "$resp" | jq -r '.success')" != "true" ]; then
    c_fail "写入跳转规则失败：$(printf '%s' "$resp" | jq -c '.errors')"; return 1
  fi
  c_ok "302 跳转规则已更新：$fqdn -> $target"
  printf '%s' "$rs_id"
}

# ── DDNS 模式 ─────────────────────────────────────────────────────────────
if [ "$DDNS" -eq 1 ]; then
  [ -f "$CONFIG_PATH" ] || { c_fail "未找到 $CONFIG_PATH，请先运行交互式配置"; exit 1; }
  TOKEN=$(jq -r '.token' "$CONFIG_PATH")
  ZONE_ID=$(jq -r '.zone_id' "$CONFIG_PATH")
  FQDN=$(jq -r '.fqdn' "$CONFIG_PATH")
  MODE_CFG=$(jq -r '.mode' "$CONFIG_PATH")
  PORT_CFG=$(jq -r '.port' "$CONFIG_PATH")
  SCHEME_CFG=$(jq -r '.scheme' "$CONFIG_PATH")
  ip6="$IP"
  if [ -z "$ip6" ]; then
    ip6=$(detect_ipv6 | head -n1)
    [ -z "$ip6" ] && { c_fail "未检测到公网 IPv6（网络变化？）"; exit 1; }
  fi
  proxied=false; [ "$MODE_CFG" = "redirect" ] && proxied=true
  upsert_aaaa "$ZONE_ID" "$FQDN" "$ip6" "$proxied" >/dev/null || exit 1
  if [ "$MODE_CFG" = "redirect" ]; then
    upsert_redirect_rule "$ZONE_ID" "$FQDN" "$SCHEME_CFG://[$ip6]:$PORT_CFG" >/dev/null || exit 1
  fi
  exit 0
fi

# ── 交互式配置 ────────────────────────────────────────────────────────────
printf '\033[36m============================================================\n ilink-wm × Cloudflare IPv6 直连 配置向导\n============================================================\033[0m\n'
cat <<'EOF'
两种模式（数据均不经过 Cloudflare）：
  1) direct   （推荐）DNS AAAA 直连——访问 http://子域.你的域名:端口 即直达
              本机 IPv6。地址栏显示域名，无证书告警（http），支持 DDNS 自动更新。
  2) redirect 302 跳转——访问 https://子域.你的域名，CF 返回 302 跳到
              http(s)://[IPv6]:端口。入口域名走 CF 代理（仅一跳 302，无业务数据），
              落地地址栏显示裸 IPv6；若用 https 目标会有证书告警。
EOF

if [ -f "$CONFIG_PATH" ]; then
  c_warn "检测到已有配置 $CONFIG_PATH"
  re=$(read_default "r=刷新DDNS / n=重新配置" "r")
  if [ "$re" = "r" ] || [ "$re" = "R" ]; then
    "$0" --ddns
    exit $?
  fi
fi

# 1. 模式
if [ "$MODE" != "direct" ] && [ "$MODE" != "redirect" ]; then
  m=$(read_choice "选择模式" "direct（推荐）" "redirect（302）")
  case "$m" in
    direct*) MODE="direct" ;;
    *)       MODE="redirect" ;;
  esac
fi

# 2. API Token（浏览器内创建，即"浏览器授权登录"）
if [ -z "$TOKEN" ] && [ "$YES" -eq 0 ]; then
  c_step "创建 Cloudflare API Token（一次性，浏览器操作）"
  cat <<EOF
  1. 脚本即将打开 https://dash.cloudflare.com/profile/api-tokens
     （未登录会先跳登录页，用你的 Cloudflare 账号登录）
  2. 点击 [Create Token] → 找到 [Create Custom Token] 点击 [Get started]
  3. 权限（Permissions）添加三行：
       Zone / Zone / Read
       Zone / DNS / Edit
       Zone / Rulesets / Edit   ← redirect 模式必需
  4. Zone Resources 选 Include → Specific zone → 你的域名
  5. Continue → Create Token → 复制生成的 Token 粘贴到下方
EOF
  url="https://dash.cloudflare.com/profile/api-tokens"
  for opener in xdg-open open; do
    if command -v "$opener" >/dev/null 2>&1; then "$opener" "$url" >/dev/null 2>&1 && break; fi
  done
  [ -t 0 ] || { c_fail "stdin 非终端，无法交互输入 Token；请用 --token 或环境变量 CF_API_TOKEN"; exit 1; }
  printf '粘贴 API Token: ' >&2
  read -rs TOKEN
  printf '\n' >&2
fi
[ -n "$TOKEN" ] || { c_fail "缺少 API Token（--token 或环境变量 CF_API_TOKEN）"; exit 1; }

c_step "验证 Token"
v=$(cf_request GET "/user/tokens/verify")
[ "$(printf '%s' "$v" | jq -r '.success')" = "true" ] || { c_fail "Token 验证失败：$(printf '%s' "$v" | jq -c '.errors')"; exit 1; }
c_ok "Token 有效"

# 3. 选择域名（zone）
c_step "获取域名列表"
zones=$(cf_request GET "/zones?per_page=50")
zone_count=$(printf '%s' "$zones" | jq -r '.result | length')
[ "$zone_count" -gt 0 ] || { c_fail "账号下没有域名（zone），请先在 Cloudflare 添加站点"; exit 1; }
if [ -n "$ZONE" ]; then
  ZONE_ID=$(printf '%s' "$zones" | jq -r --arg z "$ZONE" '.result[] | select(.name==$z) | .id' | head -n1)
  [ -n "$ZONE_ID" ] || { c_fail "未找到域名 $ZONE"; exit 1; }
  ZONE_NAME="$ZONE"
elif [ "$zone_count" -eq 1 ] || [ "$YES" -eq 1 ]; then
  ZONE_ID=$(printf '%s' "$zones" | jq -r '.result[0].id')
  ZONE_NAME=$(printf '%s' "$zones" | jq -r '.result[0].name')
else
  names=$(printf '%s' "$zones" | jq -r '.result[].name')
  name_arr=()
  while IFS= read -r line; do name_arr+=("$line"); done <<<"$names"
  picked=$(read_choice "选择要绑定的域名" "${name_arr[@]}")
  ZONE_NAME="$picked"
  ZONE_ID=$(printf '%s' "$zones" | jq -r --arg z "$ZONE_NAME" '.result[] | select(.name==$z) | .id' | head -n1)
fi
c_ok "域名：$ZONE_NAME"

# 4. 子域名
[ -n "$LABEL" ] || LABEL=$(read_default "子域名前缀" "ilink")
LABEL=$(printf '%s' "$LABEL" | tr 'A-Z' 'a-z' | tr -cd 'a-z0-9-')
FQDN="$LABEL.$ZONE_NAME"

# 5. 端口 / 协议
[ -n "$PORT" ] || PORT=$(read_default "ilink-wm Web 端口" "8888")
if [ "$MODE" = "redirect" ]; then
  if [ "$SCHEME" != "http" ] && [ "$SCHEME" != "https" ]; then
    SCHEME=$(read_default "跳转目标协议 http/https（应用无内置 TLS，http 无告警）" "http")
  fi
  [ "$SCHEME" = "http" ] || [ "$SCHEME" = "https" ] || SCHEME=http
else
  SCHEME=http
fi

# 6. IPv6 地址
ip6="$IP"
if [ -z "$ip6" ]; then
  c_step "检测本机公网 IPv6"
  if [ "$YES" -eq 1 ]; then
    ip6=$(detect_ipv6 | head -n1)
  else
    cands=$(detect_ipv6)
    if [ -z "$cands" ]; then
      c_fail "未检测到公网 IPv6 地址。可能原因：运营商未分配 / 路由器 IPv6 防火墙 / 无 IPv6 上行。"
      c_info "可用 --ip <IPv6> 手动指定后重跑。"
      exit 1
    fi
    n=$(printf '%s\n' "$cands" | wc -l)
    if [ "$n" -eq 1 ]; then
      ip6="$cands"
    else
      ip_arr=()
      while IFS= read -r line; do ip_arr+=("$line"); done <<<"$cands"
      ip6=$(read_choice "选择地址" "${ip_arr[@]}")
    fi
  fi
  [ -n "$ip6" ] || { c_fail "未检测到公网 IPv6 地址（--ip 手动指定）"; exit 1; }
fi
c_ok "IPv6 地址：$ip6"

# 7. 应用配置
c_step "写入 Cloudflare 配置"
proxied=false; [ "$MODE" = "redirect" ] && proxied=true
record_id=$(upsert_aaaa "$ZONE_ID" "$FQDN" "$ip6" "$proxied") || exit 1
rule_id=""
if [ "$MODE" = "redirect" ]; then
  rule_id=$(upsert_redirect_rule "$ZONE_ID" "$FQDN" "$SCHEME://[$ip6]:$PORT") || exit 1
fi

umask 077   # 配置含 token，限制权限
jq -n --arg token "$TOKEN" --arg zone_id "$ZONE_ID" --arg zone_name "$ZONE_NAME" \
      --arg fqdn "$FQDN" --arg mode "$MODE" --arg port "$PORT" --arg scheme "$SCHEME" \
      --arg record_id "$record_id" --arg rule_id "$rule_id" \
  '{token:$token,zone_id:$zone_id,zone_name:$zone_name,fqdn:$fqdn,mode:$mode,
    port:($port|tonumber),scheme:$scheme,record_id:$record_id,rule_id:$rule_id}' \
  > "$CONFIG_PATH"
c_info "配置已保存：$CONFIG_PATH（已 chmod 600）"

# 8. 定时 DDNS
c_step "设置定时 DDNS（每小时自动刷新 DNS/跳转规则）"
want=$(read_default "添加 cron 条目? y/n（输出到 cf-ddns.log）" "y")
if [ "$want" = "y" ] || [ "$want" = "Y" ]; then
  cron_line="@hourly $SCRIPT_DIR/$(basename "$0") --ddns >> $SCRIPT_DIR/cf-ddns.log 2>&1"
  if command -v crontab >/dev/null 2>&1; then
    (crontab -l 2>/dev/null | grep -v "ilink.*cf-setup.*--ddns"; echo "$cron_line") | crontab - \
      && c_ok "cron 已添加（查看：crontab -l）" \
      || c_warn "crontab 写入失败，请手动添加：$cron_line"
  else
    c_warn "未找到 crontab（Termux 需 pkg install cronie termux-services），请手动添加："
    c_warn "$cron_line"
  fi
fi

# 9. 防火墙提示（direct 模式）
if [ "$MODE" = "direct" ]; then
  c_step "防火墙放行入站 TCP $PORT"
  if command -v ufw >/dev/null 2>&1; then
    c_warn "检测到 ufw，如需放行：sudo ufw allow $PORT/tcp"
  elif command -v firewall-cmd >/dev/null 2>&1; then
    c_warn "检测到 firewalld，如需放行：sudo firewall-cmd --permanent --add-port=$PORT/tcp && sudo firewall-cmd --reload"
  else
    c_warn "请确认本机防火墙/安全组已放行入站 TCP $PORT"
  fi
fi

# 10. 汇总
printf '\033[32m============================================================\n 配置完成！\033[0m\n'
if [ "$MODE" = "direct" ]; then
  printf ' 访问地址： http://%s:%s   （浏览器直达本机 IPv6，数据不经 CF）\n' "$FQDN" "$PORT"
else
  printf ' 访问地址： https://%s  → 302 → %s://[%s]:%s\n' "$FQDN" "$SCHEME" "$ip6" "$PORT"
  printf '   （302 由 CF 边缘返回，业务数据直达本机，不经 CF）\n'
fi
printf ' 手动刷新 DDNS： %s --ddns\n' "$0"
printf '\n \033[33m⚠ ilink-wm 侧还需确认：\033[0m\n'
printf '   1) 服务以双栈监听：ILINK_HOST=[::]（或向导选 3），否则 IPv6 进不来\n'
printf '   2) 光猫/路由器 IPv6 防火墙需放行入站 TCP %s（很多设备默认全拦）\n' "$PORT"
printf '   3) 有公网 IPv6 直连后，serveo 隧道可以不用了（管理面板可停用）\n'
printf '\033[32m============================================================\033[0m\n'
