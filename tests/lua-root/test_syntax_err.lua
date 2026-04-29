function foo()
    local y = 2
    local Class = Actor:GetClass()
    local ParentClass = Actor.ParentClass
    while true do
        Class = ParentClass
        if true then
            break
        end
    end
end

