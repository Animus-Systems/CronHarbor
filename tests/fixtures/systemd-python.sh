#!/bin/sh
case "$4" in
  *'Prove a scheduled StartUnit frontier'*)
    systemctl="$(dirname "$0")/systemctl"
    if "$systemctl" "--$5" --no-block start --job-mode=fail "$6" >/dev/null; then
      if [ -f "$systemctl.foreign-start" ]; then
        printf '{"kind":"uncertain"}\n'
      else
        printf '{"kind":"started","jobId":17,"invocationId":"11111111111111111111111111111111"}\n'
      fi
    else
      printf '{"kind":"uncertain"}\n'
    fi
    ;;
  *) printf 'READY\n'; cat >/dev/null;;
esac
