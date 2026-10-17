on run argv
      try
        set answer to display dialog (item 1 of argv) default answer "" with hidden answer buttons {"Cancel", "Save password"} default button "Save password" cancel button "Cancel" with title "Remote Desk — SFTP password"
        return "saved:" & text returned of answer
      on error number -128
        return "canceled"
      end try
    end run
