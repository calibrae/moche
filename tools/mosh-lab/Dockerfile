# Throwaway lab: sshd + stock distro mosh-server, for testing moche end to end.
FROM debian:stable-slim
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      openssh-server mosh tcpdump iproute2 locales vim-tiny htop less ncurses-term procps tmux \
    && sed -i 's/^# *en_US.UTF-8/en_US.UTF-8/' /etc/locale.gen && locale-gen \
    && rm -rf /var/lib/apt/lists/*
ENV LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8
RUN useradd -m -s /bin/bash moche && usermod -p '*' moche \
    && mkdir -p /run/sshd /home/moche/.ssh && chmod 700 /home/moche/.ssh \
    && chown moche:moche /home/moche/.ssh \
    && ssh-keygen -A \
    && printf '%s\n' \
       'PasswordAuthentication no' 'KbdInteractiveAuthentication no' \
       'PubkeyAuthentication yes' 'PermitRootLogin no' 'UsePAM no' \
       'AllowUsers moche' 'AcceptEnv LANG LC_*' > /etc/ssh/sshd_config.d/lab.conf
# authorized_keys is copied in at run time by run.sh (docker cp), never baked in.
EXPOSE 22/tcp 60000-60010/udp
CMD ["/usr/sbin/sshd", "-D", "-e"]
