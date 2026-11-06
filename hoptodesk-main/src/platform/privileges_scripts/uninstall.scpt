set sh1 to "launchctl unload -w /Library/LaunchDaemons/com.hoptodesk.HopToDesk_service.plist 2>/dev/null;"
set sh2 to "/bin/rm -f /Library/LaunchDaemons/com.hoptodesk.HopToDesk_service.plist;"
set sh3 to "/bin/rm -f /Library/LaunchAgents/com.hoptodesk.HopToDesk_server.plist;"

set sh to sh1 & sh2 & sh3
do shell script sh with prompt "HopToDesk wants to remove the service" with administrator privileges
