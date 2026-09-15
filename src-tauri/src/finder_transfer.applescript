use framework "Foundation"
use scripting additions

on run argv
    set destinationFolder to POSIX file (item 1 of argv)
    set copyItems to {}
    set moveItems to {}
    repeat with i from 2 to count argv by 2
        set operation to item i of argv
        set sourceItem to POSIX file (item (i + 1) of argv)
        if operation is "copy" then
            set end of copyItems to sourceItem
        else if operation is "move" then
            set end of moveItems to sourceItem
        else
            error "Unsupported transfer operation"
        end if
    end repeat
    set resultPaths to {}
    -- Send lists, not individual file commands: Finder authorizes each batch.
    -- A mixed drag needs one copy operation and one move operation.
    with timeout of 604800 seconds
        tell application "Finder"
            activate
            set transferredItems to {}
            if (count copyItems) > 0 then
                set transferredItems to (duplicate copyItems to destinationFolder without replacing) as list
            end if
            if (count moveItems) > 0 then
                set transferredItems to transferredItems & ((move moveItems to destinationFolder without replacing) as list)
            end if
            repeat with transferredItem in transferredItems
                set end of resultPaths to POSIX path of (transferredItem as alias)
            end repeat
        end tell
    end timeout
    -- JSON preserves quotes, Unicode, and newlines in filenames.
    set jsonData to current application's NSJSONSerialization's dataWithJSONObject:resultPaths options:0 |error|:(missing value)
    return (current application's NSString's alloc()'s initWithData:jsonData encoding:(current application's NSUTF8StringEncoding)) as text
end run
