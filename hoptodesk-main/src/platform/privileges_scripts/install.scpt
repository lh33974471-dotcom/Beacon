on run {agent_file}

  set sh1 to "launchctl bootout system/com.hoptodesk.HopToDesk_service 2>/dev/null;"

  set sh2 to "launchctl unload -w /Library/LaunchDaemons/com.hoptodesk.HopToDesk_service.plist 2>/dev/null;"

  set sh3 to "/bin/rm -f /Library/LaunchDaemons/com.hoptodesk.HopToDesk_service.plist;"

  set sh4 to "/bin/rm -f /tmp/hoptodesk_service.err /tmp/hoptodesk_service.out;"

  set sh5 to "echo " & quoted form of agent_file & " > /Library/LaunchAgents/com.hoptodesk.HopToDesk_server.plist && chown root:wheel /Library/LaunchAgents/com.hoptodesk.HopToDesk_server.plist;"

  set sh to sh1 & sh2 & sh3 & sh4 & sh5

  do shell script sh with prompt "HopToDesk wants to install the service" with administrator privileges
end run
