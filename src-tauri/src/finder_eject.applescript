on run argv
    set mountedDisk to POSIX file (item 1 of argv)
    with timeout of 120 seconds
        tell application "Finder"
            activate
            eject mountedDisk
        end tell
    end timeout
end run
