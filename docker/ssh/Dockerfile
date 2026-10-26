# An SSH server to test tunnels through (docker-compose.yml, service ssh).
FROM docker.io/library/alpine:3.22
RUN apk add --no-cache openssh-server \
 && ssh-keygen -A \
 && adduser -D -s /bin/sh pythia \
 && echo pythia:pythia | chpasswd \
 && sed -i \
      -e 's/^#\?AllowTcpForwarding .*/AllowTcpForwarding yes/' \
      -e 's/^#\?PasswordAuthentication .*/PasswordAuthentication yes/' \
      /etc/ssh/sshd_config
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
